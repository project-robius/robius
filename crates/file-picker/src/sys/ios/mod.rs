use std::{
    cell::Cell,
    ffi::{CStr, OsStr},
    fs,
    path::{Path, PathBuf},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};
use block2::RcBlock;
use dispatch2::run_on_main;
use objc2::{
    define_class, extern_class, extern_conformance, extern_methods, extern_protocol, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, ProtocolObject},
    AnyThread, ClassType, DeclaredClass, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{
    NSArray, NSError, NSItemProvider, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_photos_ui::{
    PHPickerConfiguration, PHPickerConfigurationAssetRepresentationMode, PHPickerFilter,
    PHPickerResult,
};
use objc2_ui_kit::{
    UIApplication, UIAdaptivePresentationControllerDelegate, UIDocumentPickerDelegate,
    UIDocumentPickerViewController, UIPresentationController, UIResponder, UIViewController,
    UIWindow,
};
use objc2_uniform_type_identifiers::{UTType, UTTypeImage, UTTypeItem, UTTypeMovie};

use crate::{
    DialogCallback, DialogData, DialogOptions, Error, PickedFile, FileFilter, MediaKind, Result,
    StartLocation, DEFAULT_IMAGE_EXTENSIONS, DEFAULT_VIDEO_EXTENSIONS,
};

extern_protocol!(
    pub unsafe trait PHPickerViewControllerDelegate:
        NSObjectProtocol + MainThreadOnly
    {
        #[unsafe(method(picker:didFinishPicking:))]
        #[unsafe(method_family = none)]
        unsafe fn picker_did_finish_picking(
            &self,
            picker: &PHPickerViewController,
            results: &NSArray<PHPickerResult>,
        );
    }
);

extern_class!(
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[derive(Debug, PartialEq, Eq, Hash)]
    pub struct PHPickerViewController;
);

extern_conformance!(
    unsafe impl NSObjectProtocol for PHPickerViewController {}
);

impl PHPickerViewController {
    extern_methods!(
        #[unsafe(method(setDelegate:))]
        #[unsafe(method_family = none)]
        pub unsafe fn set_delegate(
            &self,
            delegate: Option<&ProtocolObject<dyn PHPickerViewControllerDelegate>>,
        );

        #[unsafe(method(initWithConfiguration:))]
        #[unsafe(method_family = init)]
        pub unsafe fn init_with_configuration(
            this: Allocated<Self>,
            configuration: &PHPickerConfiguration,
        ) -> Retained<Self>;
    );
}

pub(crate) fn read_uri_bytes(_uri: &str) -> Result<Vec<u8>> {
    // iOS pickers typically return regular fs paths, so this is an error case
    Err(Error::Unsupported)
}

pub(crate) fn app_temp_dir() -> Result<PathBuf> {
    Ok(std::env::temp_dir())
}

/// On iOS, `$HOME` is the app container root, so this returns directories
/// that might exist as a best effort attempt.
/// Typically, only "Documents" actually exists on most iOS devices.
fn resolve_start_location(location: StartLocation) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let sub = match location {
        StartLocation::Documents => "Documents",
        StartLocation::Downloads => "Downloads",
        StartLocation::Pictures => "Pictures",
        StartLocation::Music => "Music",
        StartLocation::Videos => "Movies",
        StartLocation::Desktop => "Desktop",
    };
    Some(PathBuf::from(home).join(sub))
}

pub(crate) fn copy_uri_to_path(_uri: &str, _dest: &Path) -> Result<()> {
    Err(Error::Unsupported)
}

pub(crate) fn pick_file(options: DialogOptions, on_completion: DialogCallback) -> Result<()> {
    show(options, DialogKind::Open, on_completion)
}

pub(crate) fn save_to_downloads(
    options: DialogOptions,
    source_path: PathBuf,
    on_completion: DialogCallback,
) -> Result<()> {
    // iOS has no Downloads directory that's accessible to apps,
    // so we simply show the system document export picker.
    show(options, DialogKind::Save { source_path }, on_completion)
}

pub(crate) fn save_data(
    options: DialogOptions,
    data: DialogData,
    on_completion: DialogCallback,
) -> Result<()> {
    let file_name = options.output_file_name_only()?;
    let temp_path = create_temporary_file(&file_name, (*data).as_ref())?;
    // If showing the picker fails, our custom delegate will never take ownership
    // of the temp file, so we clean it up here.
    let temp_dir = temp_path.parent().map(Path::to_owned);
    let result = run_on_main(move |mtm| {
        show_inner(
            mtm,
            options,
            DialogKind::SaveData { temp_path },
            on_completion,
        )
    });
    if result.is_err() {
        if let Some(dir) = temp_dir {
            let _ = fs::remove_dir_all(&dir);
        }
    }
    result
}

pub(crate) fn pick_media(
    options: DialogOptions,
    media_kind: MediaKind,
    on_completion: DialogCallback,
) -> Result<()> {
    run_on_main(move |mtm| show_media_inner(mtm, options, media_kind, on_completion))
}

enum DialogKind {
    Open,
    Save { source_path: PathBuf },
    /// This is just like `Save`, but the source file was a temp file
    /// created internally and must be cleaned up after the picker completes.
    SaveData { temp_path: PathBuf },
}

struct PendingPicker {
    on_completion: Option<DialogCallback>,
    _picker: Retained<UIDocumentPickerViewController>,
    _delegate: Retained<RobiusDocumentPickerDelegate>,
    temp_path: Option<PathBuf>,
}

struct PendingMediaPicker {
    on_completion: Option<DialogCallback>,
    _picker: Retained<PHPickerViewController>,
    _delegate: Retained<RobiusMediaPickerDelegate>,
    media_kind: MediaKind,
}

pub(super) struct Ivars {
    pending: Cell<*mut PendingPicker>,
}

pub(super) struct MediaIvars {
    pending: Cell<*mut PendingMediaPicker>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub(super) struct RobiusDocumentPickerDelegate;

    unsafe impl NSObjectProtocol for RobiusDocumentPickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for RobiusDocumentPickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        #[allow(non_snake_case)]
        unsafe fn documentPicker_didPickDocumentsAtURLs(
            &self,
            _: &UIDocumentPickerViewController,
            urls: &NSArray<NSURL>,
        ) {
            let pending = self.ivars().pending.get();
            let result_owns_temp = pending.is_null() || unsafe { (*pending).temp_path.is_none() };
            let result = urls.iter().next()
                .map(|url| Ok(Some(file_from_url(&url, result_owns_temp))))
                .unwrap_or(Err(Error::Unknown));
            self.finish(result);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        #[allow(non_snake_case)]
        unsafe fn documentPickerWasCancelled(&self, _: &UIDocumentPickerViewController) {
            self.finish(Ok(None));
        }
    }

    unsafe impl UIAdaptivePresentationControllerDelegate for RobiusDocumentPickerDelegate {
        #[unsafe(method(presentationControllerDidDismiss:))]
        #[allow(non_snake_case)]
        unsafe fn presentationControllerDidDismiss(&self, _: &UIPresentationController) {
            self.finish(Ok(None));
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MediaIvars]
    pub(super) struct RobiusMediaPickerDelegate;

    unsafe impl NSObjectProtocol for RobiusMediaPickerDelegate {}

    unsafe impl PHPickerViewControllerDelegate for RobiusMediaPickerDelegate {
        #[unsafe(method(picker:didFinishPicking:))]
        unsafe fn picker_did_finish_picking(
            &self,
            picker: &PHPickerViewController,
            results: &NSArray<PHPickerResult>,
        ) {
            self.finish(picker, results);
        }
    }

    unsafe impl UIAdaptivePresentationControllerDelegate for RobiusMediaPickerDelegate {
        #[unsafe(method(presentationControllerDidDismiss:))]
        #[allow(non_snake_case)]
        unsafe fn presentationControllerDidDismiss(&self, _: &UIPresentationController) {
            self.cancel();
        }
    }
);

impl RobiusDocumentPickerDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            pending: Cell::new(ptr::null_mut()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn set_pending(&self, pending: *mut PendingPicker) {
        self.ivars().pending.set(pending);
    }

    fn finish(&self, result: Result<Option<PickedFile>>) {
        let pending = self.ivars().pending.replace(ptr::null_mut());
        if pending.is_null() { return; }

        // SAFE: the pending picker was allocated using `Box::into_raw`
        // in `show_inner()`, and the delegate cleans it up.
        let mut pending = unsafe { Box::from_raw(pending) };

        if let Some(temp_path) = pending.temp_path.take() {
            let _ = fs::remove_file(&temp_path);
            if let Some(parent) = temp_path.parent() {
                let _ = fs::remove_dir(parent);
            }
        }

        if let Some(on_completion) = pending.on_completion.take() {
            // Run on a background thread so the callback can safely block
            // without freezing the main UI thread.
            std::thread::spawn(move || on_completion(result));
        }
    }
}

impl RobiusMediaPickerDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MediaIvars {
            pending: Cell::new(ptr::null_mut()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn set_pending(&self, pending: *mut PendingMediaPicker) {
        self.ivars().pending.set(pending);
    }

    fn cancel(&self) {
        let pending = self.ivars().pending.replace(ptr::null_mut());
        if !pending.is_null() {
            finish_media_picker(pending, Ok(None));
        }
    }

    fn finish(&self, picker: &PHPickerViewController, results: &NSArray<PHPickerResult>) {
        let pending = self.ivars().pending.replace(ptr::null_mut());
        dismiss_media_picker(picker);
        if pending.is_null() { return; }

        // SAFE: we alloc'd `pending` as a Box and set the ptr value via `Box::into_raw`,
        //       and then we just tool ownership of it with `replace` above.
        let media_kind = unsafe { (*pending).media_kind };

        let Some(result) = results.iter().next() else {
            finish_media_picker(pending, Ok(None));
            return;
        };

        let provider = unsafe { result.itemProvider() };
        let type_identifier = match media_type_identifier(&provider, media_kind) {
            Some(type_identifier) => type_identifier,
            None => {
                finish_media_picker(pending, Err(Error::Unknown));
                return;
            }
        };
        let suggested_name = provider.suggestedName().map(|n| n.to_string());

        // The block below captures `provider` and a `MediaLoadGuard` that owns
        // the `pending` picker. This ensures that the pending picker is dropped
        // precisely once and the callback also runs exactly once.
        let guard = MediaLoadGuard { pending: Cell::new(pending) };
        let provider_for_call = provider.clone();
        let block = RcBlock::new(move |url: *mut NSURL, error: *mut NSError| {
            let _provider = &provider;
            let pending = guard.take();
            if pending.is_null() { return; }
            let result = if !error.is_null() {
                Err(Error::Unknown)
            } else if url.is_null() {
                Err(Error::Unknown)
            } else {
                // SAFE: `NSItemProvider` always supplies a valid file URL.
                copy_media_file(unsafe { &*url }, suggested_name.as_deref()).map(Some)
            };
            finish_media_picker(pending, result);
        });

        // SAFE: the retval here is only needed for observation/cancellation,
        // so we can just drop it since it won't cancel the task or completion handler.
        let _ = unsafe {
            provider_for_call.loadFileRepresentationForTypeIdentifier_completionHandler(
                &type_identifier,
                &block,
            )
        };
    }
}

fn show(options: DialogOptions, kind: DialogKind, on_completion: DialogCallback) -> Result<()> {
    run_on_main(move |mtm| show_inner(mtm, options, kind, on_completion))
}

fn show_inner(
    mtm: MainThreadMarker,
    options: DialogOptions,
    kind: DialogKind,
    on_completion: DialogCallback,
) -> Result<()> {
    let presenter = presenting_view_controller(mtm)?;
    let delegate = RobiusDocumentPickerDelegate::new(mtm);

    let (picker, temp_path) = match kind {
        DialogKind::Open => {
            let content_types = content_types(&options);
            let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
                UIDocumentPickerViewController::alloc(mtm),
                &content_types,
                true,
            );
            (picker, None)
        }
        DialogKind::Save { source_path } => {
            let (export_path, temp_path) = export_source_path(&options, &source_path)?;
            let url = file_url(&export_path)?;
            let urls = NSArray::from_retained_slice(&[url]);
            let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(
                UIDocumentPickerViewController::alloc(mtm),
                &urls,
                true,
            );
            (picker, temp_path)
        }
        DialogKind::SaveData { temp_path } => {
            let url = file_url(&temp_path)?;
            let urls = NSArray::from_retained_slice(&[url]);
            let picker = UIDocumentPickerViewController::initForExportingURLs_asCopy(
                UIDocumentPickerViewController::alloc(mtm),
                &urls,
                true,
            );
            (picker, Some(temp_path))
        }
    };

    picker.setAllowsMultipleSelection(false);
    picker.setShouldShowFileExtensions(true);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    if let Some(title) = options.title.as_deref() {
        picker.setTitle(Some(&NSString::from_str(title)));
    }

    let initial_dir = options.directory.clone().or_else(
        || options.start_location.and_then(resolve_start_location)
    );
    if let Some(directory) = initial_dir {
        if let Ok(url) = file_url(&directory) {
            picker.setDirectoryURL(Some(&url));
        }
    }

    let pending = Box::new(PendingPicker {
        on_completion: Some(on_completion),
        _picker: picker.clone(),
        _delegate: delegate.clone(),
        temp_path,
    });
    delegate.set_pending(Box::into_raw(pending));

    // Safe upcast: `UIDocumentPickerViewController`'s superclass is `UIViewController`.
    let picker_view_controller: Retained<UIViewController> = picker.into_super();
    let presentation_delegate = ProtocolObject::from_ref(&*delegate);
    set_presentation_delegate(&picker_view_controller, presentation_delegate);
    presenter.presentViewController_animated_completion(&picker_view_controller, true, None);
    set_presentation_delegate(&picker_view_controller, presentation_delegate);

    Ok(())
}

fn show_media_inner(
    mtm: MainThreadMarker,
    options: DialogOptions,
    media_kind: MediaKind,
    on_completion: DialogCallback,
) -> Result<()> {
    let picker_class = CStr::from_bytes_with_nul(b"PHPickerViewController\0").unwrap();
    if AnyClass::get(picker_class).is_none() {
        return show_inner(
            mtm,
            media_document_options(options, media_kind),
            DialogKind::Open,
            on_completion,
        );
    }

    let presenter = presenting_view_controller(mtm)?;
    let configuration = unsafe { PHPickerConfiguration::init(PHPickerConfiguration::alloc()) };
    let filter = media_filter(media_kind);

    unsafe {
        configuration.setSelectionLimit(1);
        configuration.setFilter(Some(&filter));
        configuration.setPreferredAssetRepresentationMode(
            PHPickerConfigurationAssetRepresentationMode::Current,
        );
    }

    let picker = unsafe {
        PHPickerViewController::init_with_configuration(
            PHPickerViewController::alloc(mtm),
            &configuration,
        )
    };
    let delegate = RobiusMediaPickerDelegate::new(mtm);

    unsafe {
        picker.set_delegate(Some(ProtocolObject::from_ref(&*delegate)));
    }

    if let Some(title) = options.title.as_deref() {
        picker.setTitle(Some(&NSString::from_str(title)));
    }

    let pending = Box::new(PendingMediaPicker {
        on_completion: Some(on_completion),
        _picker: picker.clone(),
        _delegate: delegate.clone(),
        media_kind,
    });
    delegate.set_pending(Box::into_raw(pending));

    // Safe upcast: `PHPickerViewController`'s superclass is `UIViewController`.
    let picker_view_controller: Retained<UIViewController> = picker.into_super();
    let presentation_delegate = ProtocolObject::from_ref(&*delegate);
    set_presentation_delegate(&picker_view_controller, presentation_delegate);
    presenter.presentViewController_animated_completion(&picker_view_controller, true, None);
    set_presentation_delegate(&picker_view_controller, presentation_delegate);

    Ok(())
}

fn set_presentation_delegate(
    controller: &UIViewController,
    delegate: &ProtocolObject<dyn UIAdaptivePresentationControllerDelegate>,
) {
    if let Some(presentation_controller) = controller.presentationController() {
        unsafe {
            presentation_controller.setDelegate(Some(delegate));
        }
    }
}

fn presenting_view_controller(mtm: MainThreadMarker) -> Result<Retained<UIViewController>> {
    let application = UIApplication::sharedApplication(mtm);
    let window = active_window(&application).ok_or(Error::Unknown)?;
    let root = window.rootViewController().ok_or(Error::Unknown)?;
    top_presenting_view_controller(root)
}

fn active_window(application: &UIApplication) -> Option<Retained<UIWindow>> {
    #[allow(deprecated)]
    if let Some(window) = application.keyWindow() {
        return Some(window);
    }

    #[allow(deprecated)]
    let windows = application.windows();
    windows
        .iter()
        .find(|window| window.isKeyWindow())
        .or_else(|| windows.iter().next())
}

fn top_presenting_view_controller(
    mut controller: Retained<UIViewController>,
) -> Result<Retained<UIViewController>> {
    loop {
        if is_file_picker_controller(&controller) {
            return Err(Error::AlreadyOpen);
        }
        let Some(presented) = controller.presentedViewController() else {
            return Ok(controller);
        };
        controller = presented;
    }
}

fn is_file_picker_controller(controller: &UIViewController) -> bool {
    if controller.isKindOfClass(UIDocumentPickerViewController::class()) {
        return true;
    }

    let picker_class = CStr::from_bytes_with_nul(b"PHPickerViewController\0").unwrap();
    AnyClass::get(picker_class)
        .map(|class| controller.isKindOfClass(class))
        .unwrap_or(false)
}

fn media_filter(media_kind: MediaKind) -> Retained<PHPickerFilter> {
    match media_kind {
        MediaKind::Image => unsafe { PHPickerFilter::imagesFilter() },
        MediaKind::Video => unsafe { PHPickerFilter::videosFilter() },
        MediaKind::ImageOrVideo => {
            let images = unsafe { PHPickerFilter::imagesFilter() };
            let videos = unsafe { PHPickerFilter::videosFilter() };
            let filters = NSArray::from_retained_slice(&[images, videos]);
            unsafe { PHPickerFilter::anyFilterMatchingSubfilters(&filters) }
        }
    }
}

fn media_document_options(
    mut options: DialogOptions,
    media_kind: MediaKind,
) -> DialogOptions {
    if !options.filters.is_empty() {
        return options;
    }
    options.mime_type = match media_kind {
        MediaKind::Image => Some("image/*".to_owned()),
        MediaKind::Video => Some("video/*".to_owned()),
        MediaKind::ImageOrVideo => None,
    };

    match media_kind {
        MediaKind::Image => options.filters.push(FileFilter {
            name: "Images".to_owned(),
            extensions: DEFAULT_IMAGE_EXTENSIONS
                .iter()
                .map(ToString::to_string)
                .collect(),
        }),
        MediaKind::Video => options.filters.push(FileFilter {
            name: "Videos".to_owned(),
            extensions: DEFAULT_VIDEO_EXTENSIONS
                .iter()
                .map(ToString::to_string)
                .collect(),
        }),
        MediaKind::ImageOrVideo => {
            options.filters.push(FileFilter {
                name: "Images".to_owned(),
                extensions: DEFAULT_IMAGE_EXTENSIONS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
            options.filters.push(FileFilter {
                name: "Videos".to_owned(),
                extensions: DEFAULT_VIDEO_EXTENSIONS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }
    }

    options
}


/// Chooses which content to load from a picked media item based on the given media kind.
///
/// Live photos are kinda annoying to deal with on iOS, which is why we no longer use
/// [`first_type_identifier`] (the first entry of `registeredTypeIdentifiers`) here,
/// because that returns some weird `.pvt` package for a live photo.
fn media_type_identifier(
    provider: &NSItemProvider,
    media_kind: MediaKind,
) -> Option<Retained<NSString>> {
    let image_uti = unsafe { UTTypeImage };
    let video_uti = unsafe { UTTypeMovie };

    let conforms_to = |ut_type: &UTType| {
        provider.hasItemConformingToTypeIdentifier(&ut_type.identifier())
    };

    // we prefer returning the still image from within a live photo,
    // but only if the caller indicated that they want an image.
    let ut_type = if media_kind.is_image() && conforms_to(image_uti) {
        image_uti
    } else if media_kind.is_video() && conforms_to(video_uti) {
        video_uti
    } else {
        // Fall back to the first registered identifier if nothing conforms.
        return provider.registeredTypeIdentifiers().iter().next();
    };
    Some(ut_type.identifier())
}

fn dismiss_media_picker(picker: &PHPickerViewController) {
    let Some(picker) = (unsafe {
        Retained::retain(picker as *const PHPickerViewController as *mut PHPickerViewController)
    }) else {
        return;
    };
    // Safe upcast: `PHPickerViewController`'s superclass is `UIViewController`.
    let picker: Retained<UIViewController> = picker.into_super();
    picker.dismissViewControllerAnimated_completion(true, None);
}

/// A wrapper guart type that just owns a `PendingMediaPicker` instance
/// while `NSItemProvider.loadFileRepresentation` is being called.
struct MediaLoadGuard {
    pending: Cell<*mut PendingMediaPicker>,
}

impl MediaLoadGuard {
    fn take(&self) -> *mut PendingMediaPicker {
        self.pending.replace(ptr::null_mut())
    }
}

impl Drop for MediaLoadGuard {
    fn drop(&mut self) {
        let pending = self.pending.replace(ptr::null_mut());
        if !pending.is_null() {
            finish_media_picker(pending, Err(Error::Unknown));
        }
    }
}

fn finish_media_picker(pending: *mut PendingMediaPicker, result: Result<Option<PickedFile>>) {
    if pending.is_null() {
        return;
    }

    // SAFE: The pending picker was allocated by `Box::into_raw` in `show_media_inner`,
    // and every completion path reclaims it at most once.
    let mut pending = unsafe { Box::from_raw(pending) };
    if let Some(on_completion) = pending.on_completion.take() {
        // Run on a background thread so the callback may block without
        // stalling whatever queue the load completion handler ran on.
        std::thread::spawn(move || on_completion(result));
    }
}

fn copy_media_file(url: &NSURL, suggested_name: Option<&str>) -> Result<PickedFile> {
    if url.isFileURL() {
        if let Some(path) = url.path() {
            let source_path = resolve_regular_file(PathBuf::from(path.to_string()))?;
            let file_name = source_path
                .file_name()
                .and_then(|name| name.to_str())
                .or(suggested_name)
                .filter(|name| !name.is_empty())
                .unwrap_or("media");
            let temp_path = create_temporary_copy(file_name, &source_path)?;
            // We created this copy, so the resulting `PickedFile` owns it and
            // `into_local_file` will clean it up after use.
            return Ok(PickedFile::from_owned_temp_path(temp_path));
        }
    }

    let uri = url.absoluteString()
        .map(|uri| uri.to_string())
        .unwrap_or_default();
    Ok(PickedFile::from_uri(uri))
}

fn content_types(options: &DialogOptions) -> Retained<NSArray<UTType>> {
    let mut types = Vec::new();

    if let Some(mime_type) = options.mime_type.as_deref().filter(|mime| !mime.is_empty()) {
        if mime_type == "image/*" {
            types.push(retained_ut_type(unsafe { UTTypeImage }));
        } else if mime_type == "video/*" {
            types.push(retained_ut_type(unsafe { UTTypeMovie }));
        } else if let Some(content_type) =
            UTType::typeWithMIMEType(&NSString::from_str(mime_type))
        {
            types.push(content_type);
        }
    }

    for extension in options
        .filters
        .iter()
        .flat_map(|filter| filter.extensions.iter())
    {
        let extension = extension.trim_start_matches('.');
        if extension.is_empty() {
            continue;
        }
        if let Some(content_type) =
            UTType::typeWithFilenameExtension(&NSString::from_str(extension))
        {
            types.push(content_type);
        }
    }

    if types.is_empty() {
        types.push(retained_ut_type(unsafe { UTTypeItem }));
    }

    NSArray::from_retained_slice(&types)
}

fn retained_ut_type(ut_type: &'static UTType) -> Retained<UTType> {
    // SAFE: `UTType*` is a core type guaranteed to be a valid singleton object.
    unsafe {
        Retained::retain(ut_type as *const UTType as *mut UTType)
            .expect("UTType singleton should not be null")
    }
}

fn export_source_path(
    options: &DialogOptions,
    source_path: &Path,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let file_name = options.output_file_name(source_path)?;
    if source_path.file_name().and_then(|name| name.to_str()) == Some(file_name.as_str()) {
        return Ok((source_path.to_owned(), None));
    }

    let temp_path = create_temporary_copy(&file_name, source_path)?;
    Ok((temp_path.clone(), Some(temp_path)))
}

fn create_temporary_copy(file_name: &str, source_path: &Path) -> Result<PathBuf> {
    let directory = unique_temp_directory()?;
    let path = directory.join(file_name);
    fs::copy(source_path, &path)?;
    Ok(path)
}

/// Resolves the given path to a regular file, handling quirks like a live photo.
///
/// If `path` points to a package bundle (like a live photo `.pvt`),
/// this function iterates through the files in that package to pull out a path to
/// (1) an image, or (2) a video, or (3) anything elae as a last resort.
fn resolve_regular_file(path: PathBuf) -> Result<PathBuf> {
    let metadata = fs::metadata(&path)?;
    if metadata.is_file() {
        return Ok(path);
    }
    if metadata.is_dir() {
        let mut best: Option<(u8, PathBuf)> = None;
        for entry in fs::read_dir(&path)?.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let priority = file_type_priority(&p);
            if best.as_ref().is_none_or(|(best_prio, _)| priority > *best_prio) {
                best = Some((priority, p));
            }
        }
        if let Some((_, file)) = best {
            return Ok(file);
        }
    }
    Err(Error::Unknown)
}

fn file_type_priority(path: &Path) -> u8 {
    let Some(ext) = path.extension().and_then(OsStr::to_str) else {
        return 1;
    };
    let Some(ut_type) = UTType::typeWithFilenameExtension(&NSString::from_str(ext)) else {
        return 1;
    };
    if unsafe { ut_type.conformsToType(UTTypeImage) } {
        3
    } else if unsafe { ut_type.conformsToType(UTTypeMovie) } {
        2
    } else {
        1
    }
}

fn create_temporary_file(file_name: &str, data: &[u8]) -> Result<PathBuf> {
    let directory = unique_temp_directory()?;
    let path = directory.join(file_name);
    fs::write(&path, data)?;
    Ok(path)
}

fn unique_temp_directory() -> Result<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("robius-file-picker-{}-{now}", std::process::id()));
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn file_url(path: &Path) -> Result<Retained<NSURL>> {
    let path = path.to_str().ok_or(Error::InvalidFileName)?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

fn file_from_url(url: &NSURL, owned_temp: bool) -> PickedFile {
    if url.isFileURL() {
        if let Some(path) = url.path() {
            let path = PathBuf::from(path.to_string());
            return if owned_temp {
                PickedFile::from_owned_temp_path(path)
            } else {
                PickedFile::from_path(path)
            };
        }
    }

    let uri = url.absoluteString()
        .map(|uri| uri.to_string())
        .unwrap_or_default();
    PickedFile::from_uri(uri)
}
