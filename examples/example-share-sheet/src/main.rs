pub use makepad_widgets;

use std::{
    env,
    fs,
    path::{Path, PathBuf},
};

use makepad_widgets::*;
use robius_file_picker::{FileDialog, PickedFile};
use robius_share::ShareSheet;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.inner_size: vec2(520, 430)
                body +: {
                    main_view := View{
                        width: Fill
                        height: Fill
                        flow: Down
                        spacing: 12
                        padding: 18

                        Label{
                            text: "robius-share example"
                            draw_text.text_style.font_size: 24
                        }

                        Label{
                            width: Fill
                            text: "Use these buttons to present the native share sheet from a real Makepad window."
                            draw_text.text_style.font_size: 10
                            draw_text.color: #777
                        }

                        share_text_button := Button{
                            width: Fill
                            text: "Share Text"
                        }

                        share_url_button := Button{
                            width: Fill
                            text: "Share URL"
                        }

                        share_file_button := Button{
                            width: Fill
                            text: "Share Generated File"
                        }

                        share_mixed_button := Button{
                            width: Fill
                            text: "Share Mixed Payload"
                        }

                        pick_and_share_file_button := Button{
                            width: Fill
                            text: "Pick File and Share"
                        }

                        status_label := Label{
                            width: Fill
                            text: "Ready"
                            draw_text.text_style.font_size: 11
                            draw_text.color: #666
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum ShareExampleAction {
    PickedFile(std::result::Result<Option<PickedFile>, String>),
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl App {
    fn set_status(&self, cx: &mut Cx, text: &str) {
        self.ui.label(cx, ids!(status_label)).set_text(cx, text);
    }

    fn present_share(&self, cx: &mut Cx, result: robius_share::Result<()>) {
        match result {
            Ok(()) => self.set_status(cx, "Share sheet presented."),
            Err(error) => self.set_status(cx, &format!("Share failed: {error}")),
        }
    }

    fn share_text(&self, cx: &mut Cx) {
        self.present_share(
            cx,
            ShareSheet::new()
                .set_title("Robius share example")
                .set_subject("Text payload")
                .add_text("Robius share example text")
                .share(),
        );
    }

    fn share_url(&self, cx: &mut Cx) {
        self.present_share(
            cx,
            ShareSheet::new()
                .set_title("Robius share example")
                .set_subject("URL payload")
                .add_url("https://robius.rs/")
                .share(),
        );
    }

    fn share_file(&self, cx: &mut Cx) {
        match ensure_example_file() {
            Ok(path) => self.present_share(
                cx,
                ShareSheet::new()
                    .set_title("Robius share example")
                    .set_subject("File payload")
                    .add_file_with_mime_type(path, "text/plain")
                    .share(),
            ),
            Err(error) => self.set_status(cx, &format!("Could not create file: {error}")),
        }
    }

    fn share_mixed(&self, cx: &mut Cx) {
        match ensure_example_file() {
            Ok(path) => self.present_share(
                cx,
                ShareSheet::new()
                    .set_title("Robius share example")
                    .set_subject("Mixed payload")
                    .add_text("Robius share example mixed payload")
                    .add_url("https://robius.rs/")
                    .add_file_with_mime_type(path, "text/plain")
                    .share(),
            ),
            Err(error) => self.set_status(cx, &format!("Could not create file: {error}")),
        }
    }

    fn pick_and_share_file(&self, cx: &mut Cx) {
        self.set_status(cx, "Opening file picker...");
        let result = FileDialog::new()
            .set_title("Pick a file to share")
            .pick_file(|result| {
                let action = match result {
                    Ok(file) => ShareExampleAction::PickedFile(Ok(file)),
                    Err(error) => ShareExampleAction::PickedFile(Err(error.to_string())),
                };
                Cx::post_action(action);
            });

        if let Err(error) = result {
            self.set_status(cx, &format!("File picker failed: {error}"));
        }
    }

    fn share_picked_file(&self, cx: &mut Cx, picked_file: PickedFile) {
        let display_name = picked_file.file_name().unwrap_or("picked file");
        let title = format!("Picked file: {display_name}");

        let share = if let Some(path) = picked_file.path() {
            let mut share = ShareSheet::new()
                .set_title("Robius share example")
                .set_subject(title);
            if let Some(mime_type) = picked_file.mime_type() {
                share = share.add_file_with_mime_type(path, mime_type);
            } else {
                share = share.add_file(path);
            }
            share
        } else if let Some(uri) = picked_file.uri() {
            let mut share = ShareSheet::new()
                .set_title("Robius share example")
                .set_subject(title);
            if let Some(mime_type) = picked_file.mime_type() {
                share = share.add_file_uri_with_mime_type(uri, mime_type);
            } else {
                share = share.add_file_uri(uri);
            }
            share
        } else {
            self.set_status(cx, "Picked file did not include a path or URI.");
            return;
        };

        self.present_share(cx, share.share());
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if let Some(action) = action.downcast_ref::<ShareExampleAction>() {
                match action {
                    ShareExampleAction::PickedFile(Ok(Some(file))) => {
                        self.share_picked_file(cx, file.clone());
                    }
                    ShareExampleAction::PickedFile(Ok(None)) => {
                        self.set_status(cx, "File picker cancelled.");
                    }
                    ShareExampleAction::PickedFile(Err(error)) => {
                        self.set_status(cx, &format!("File picker failed: {error}"));
                    }
                }
            }
        }

        if self.ui.button(cx, ids!(share_text_button)).clicked(actions) {
            self.share_text(cx);
        }
        if self.ui.button(cx, ids!(share_url_button)).clicked(actions) {
            self.share_url(cx);
        }
        if self.ui.button(cx, ids!(share_file_button)).clicked(actions) {
            self.share_file(cx);
        }
        if self.ui.button(cx, ids!(share_mixed_button)).clicked(actions) {
            self.share_mixed(cx);
        }
        if self
            .ui
            .button(cx, ids!(pick_and_share_file_button))
            .clicked(actions)
        {
            self.pick_and_share_file(cx);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

fn example_file_path() -> PathBuf {
    env::temp_dir().join("robius-share-example.txt")
}

fn ensure_example_file() -> std::io::Result<PathBuf> {
    let path = example_file_path();
    write_example_file(&path)?;
    Ok(path)
}

fn write_example_file(path: &Path) -> std::io::Result<()> {
    fs::write(
        path,
        "This file was generated by the robius-share Makepad example app.\n",
    )
}
