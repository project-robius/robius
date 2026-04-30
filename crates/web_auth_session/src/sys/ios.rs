use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use block2::{Block, RcBlock};
use dispatch2::{run_on_main, DispatchQueue};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, MainThreadMarker,
    MainThreadOnly,
};
use objc2_authentication_services::{
    ASPresentationAnchor, ASWebAuthenticationPresentationContextProviding,
    ASWebAuthenticationSession, ASWebAuthenticationSessionErrorCode,
    ASWebAuthenticationSessionErrorDomain,
};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIApplication, UIWindow};

use crate::{Error, Result};

// iOS 13+ wants a presentation-context provider so the auth sheet knows
// which window to anchor to. We just point it at the app's key window.
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct PresentationContextProvider;

    unsafe impl NSObjectProtocol for PresentationContextProvider {}

    unsafe impl ASWebAuthenticationPresentationContextProviding for PresentationContextProvider {
        #[unsafe(method_id(presentationAnchorForWebAuthenticationSession:))]
        #[allow(non_snake_case)]
        unsafe fn presentationAnchorForWebAuthenticationSession(
            &self,
            _session: &ASWebAuthenticationSession,
        ) -> Retained<ASPresentationAnchor> {
            let mtm = MainThreadMarker::from(self);
            let app = UIApplication::sharedApplication(mtm);

            // Deprecated for multi-scene apps, fine for single-scene.
            // Swap for a connectedScenes lookup if your app uses scenes.
            #[allow(deprecated)]
            let window: Option<Retained<UIWindow>> = unsafe { app.keyWindow() }
                .or_else(|| {
                    #[allow(deprecated)]
                    let windows = app.windows();
                    windows.iter().next()
                });

            match window {
                // ASPresentationAnchor is just NSObject; UIWindow inherits
                // from it (via UIView/UIResponder), so the upcast is sound.
                Some(window) => unsafe {
                    Retained::cast_unchecked::<ASPresentationAnchor>(window)
                },
                // No window means the session will fail with PresentationContextInvalid;
                // return *something* to satisfy the type signature.
                None => NSObject::new(),
            }
        }
    }
);

impl PresentationContextProvider {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // set_ivars(()) is needed even with no ivars; it transitions
        // Allocated to PartialInit so msg_send accepts the receiver.
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

// Heap state we leak past start() and reclaim when the completion fires.
// Holds the session retain so iOS doesn't drop it on us, and the context
// provider retain since the session stores it weakly.
struct PendingSession {
    on_completion: Box<dyn FnOnce(Result<String>) + Send + 'static>,
    session: Option<Retained<ASWebAuthenticationSession>>,
    _context_provider: Retained<PresentationContextProvider>,
}

// Pointer the completion block carries. Only touched on main, so the
// !Send retains inside don't matter; we just need the pointer Send/Sync.
#[derive(Clone, Copy)]
struct SendStatePtr(*mut PendingSession);
unsafe impl Send for SendStatePtr {}
unsafe impl Sync for SendStatePtr {}

// Same trick for the cancel handle's session pointer. The actual deref
// happens on main (cancel dispatches there before touching it).
#[derive(Clone, Copy)]
struct SendSessionPtr(*mut ASWebAuthenticationSession);
unsafe impl Send for SendSessionPtr {}
unsafe impl Sync for SendSessionPtr {}

/// Platform-specific handle behind `crate::AuthSessionHandle`.
#[derive(Clone)]
pub(crate) struct Handle {
    state: Arc<HandleState>,
}

struct HandleState {
    // True once the completion handler has run. cancel() checks this on
    // main before touching session_ptr; both run on main, so no race.
    fired: Arc<AtomicBool>,
    // True after we've dispatched cancel; debounces extra cancel() calls.
    cancel_dispatched: AtomicBool,
    // Pointer to the session held alive by `PendingSession`. Valid while
    // `fired` is false.
    session_ptr: SendSessionPtr,
}

impl Handle {
    pub(crate) fn cancel(&self) {
        if self.state.cancel_dispatched.swap(true, Ordering::SeqCst) {
            return;
        }
        let state = self.state.clone();
        // session.cancel must run on main. Dispatch async so we don't
        // block the caller, and so we don't deadlock if cancel() got
        // called from inside another main-queue task.
        DispatchQueue::main().exec_async(move || {
            // If completion already fired, the session is gone and we
            // must skip. Both on main, so no race with the block's swap.
            if !state.fired.load(Ordering::SeqCst) {
                // SAFETY: fired is false, so PendingSession still holds the
                // session retain and session_ptr is valid.
                unsafe { (*state.session_ptr.0).cancel() };
            }
        });
    }
}

pub(crate) fn start<F>(
    url: &str,
    callback_scheme: &str,
    prefers_ephemeral: bool,
    on_completion: F,
) -> Result<Handle>
where
    F: FnOnce(Result<String>) + Send + 'static,
{
    let url = url.to_owned();
    let callback_scheme = callback_scheme.to_owned();

    run_on_main(move |mtm| -> Result<Handle> {
        let url_ns = NSString::from_str(&url);
        let url_obj = unsafe { NSURL::URLWithString(&url_ns) }.ok_or(Error::MalformedUri)?;
        let scheme_ns = NSString::from_str(&callback_scheme);

        let context_provider = PresentationContextProvider::new(mtm);

        // Box up the state and leak the pointer to hand to the completion
        // block. `session` gets filled in below.
        let state = Box::new(PendingSession {
            on_completion: Box::new(on_completion),
            session: None,
            _context_provider: context_provider.clone(),
        });
        let state_ptr = SendStatePtr(Box::into_raw(state));

        // Apple says completion fires at most once, but the start-failed
        // path below also wants to claim state, so gate both with `fired`.
        let fired = Arc::new(AtomicBool::new(false));
        let fired_block = fired.clone();

        let block = RcBlock::new(move |callback_url: *mut NSURL, error: *mut NSError| {
            if fired_block.swap(true, Ordering::SeqCst) {
                return;
            }
            // SAFETY: state_ptr came from Box::into_raw, the atomic ensures
            // we're the only reclaimer, and we're on main.
            let state: PendingSession = *unsafe { Box::from_raw(state_ptr.0) };

            let result = if !error.is_null() {
                let err = unsafe { &*error };
                let domain = err.domain();
                let code = err.code();
                // SAFETY: the error-domain static is an immutable framework
                // NSString; isEqualToString is a normal NSString comparison.
                let is_our_domain = unsafe {
                    domain.isEqualToString(ASWebAuthenticationSessionErrorDomain)
                };
                if is_our_domain && code == ASWebAuthenticationSessionErrorCode::CanceledLogin.0 {
                    Err(Error::UserCancelled)
                } else {
                    let desc = err.localizedDescription().to_string();
                    Err(Error::AuthFailed(format!(
                        "{desc} (domain={domain}, code={code})"
                    )))
                }
            } else if !callback_url.is_null() {
                let url = unsafe { &*callback_url };
                Ok(unsafe { url.absoluteString() }
                    .map(|s| s.to_string())
                    .unwrap_or_default())
            } else {
                Err(Error::Unknown)
            };

            (state.on_completion)(result);
            // state drops here, releasing the session and context-provider
            // retains so they can deallocate.
        });

        // The completion-handler param is `*mut Block<dyn Fn(...)>` under
        // the hood; build that ptr from our RcBlock.
        let block_ptr = (&*block) as *const Block<dyn Fn(*mut NSURL, *mut NSError)>
            as *mut Block<dyn Fn(*mut NSURL, *mut NSError)>;

        // The non-deprecated init takes a Universal-Link callback object
        // (iOS 17.4+). For our custom-scheme case the deprecated init is
        // still the right fit.
        #[allow(deprecated)]
        let session = unsafe {
            ASWebAuthenticationSession::initWithURL_callbackURLScheme_completionHandler(
                ASWebAuthenticationSession::alloc(),
                &url_obj,
                Some(&scheme_ns),
                block_ptr,
            )
        };

        unsafe {
            session.setPresentationContextProvider(Some(ProtocolObject::from_ref(&*context_provider)));
        }
        if prefers_ephemeral {
            unsafe { session.setPrefersEphemeralWebBrowserSession(true) };
        }

        // Grab the session pointer for the cancel handle now, so cancel
        // doesn't have to chase Box->Option->Retained on its hot path.
        let session_ptr = SendSessionPtr(Retained::as_ptr(&session) as *mut _);

        // Stash the session retain so ARC keeps it alive until completion.
        unsafe {
            (*state_ptr.0).session = Some(session.clone());
        }

        let started = unsafe { session.start() };
        if !started {
            // Apple won't fire completion if start() returned NO, so we
            // have to reclaim state_ptr ourselves. The atomic guards
            // against a stray late completion double-freeing.
            if !fired.swap(true, Ordering::SeqCst) {
                let state: PendingSession = *unsafe { Box::from_raw(state_ptr.0) };
                (state.on_completion)(Err(Error::Unknown));
            }
            return Err(Error::Unknown);
        }

        Ok(Handle {
            state: Arc::new(HandleState {
                fired,
                cancel_dispatched: AtomicBool::new(false),
                session_ptr,
            }),
        })
    })
}
