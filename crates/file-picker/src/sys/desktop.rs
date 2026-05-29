use std::{fs, path::{Path, PathBuf}};
use crate::{
    DialogCallback, DialogData, DialogOptions, Error, PickedFile, MediaKind, Result, StartLocation,
    DEFAULT_IMAGE_EXTENSIONS, DEFAULT_VIDEO_EXTENSIONS,
};

// Desktop dialogs always return filesystem paths, so most of these are no-ops.

pub(crate) fn read_uri_bytes(_uri: &str) -> Result<Vec<u8>> {
    Err(Error::Unsupported)
}

pub(crate) fn app_temp_dir() -> Result<PathBuf> {
    Ok(std::env::temp_dir())
}

pub(crate) fn copy_uri_to_path(_uri: &str, _dest: &Path) -> Result<()> {
    Err(Error::Unsupported)
}

/// Shows a native file dialog, then calls the given `callback` with the result `R`.
///
/// The `callback` might block, so it always runs on a background worker thread.
/// This function is safe to run from any thread context. 
fn run_dialog_then<R, D, C>(dialog_fn: D, callback: C)
where
    R: Default + Send + 'static,
    D: FnOnce() -> R + Send + 'static,
    C: FnOnce(R) + Send + 'static,
{
    #[cfg(target_os = "macos")] {
        dispatch2::DispatchQueue::main().exec_async(move || {
            // GCD dispatches the dialog to the main UI thread in a C stack frame,
            // so we need to catch a panic/exception being unwound here
            // to prevent UB (or a process abort).
            let result = std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(dialog_fn)
            ).unwrap_or_default();
            // Post-dialog work may block, so move it off the main thread.
            std::thread::spawn(move || callback(result));
        });
    }
    #[cfg(not(target_os = "macos"))] {
        std::thread::spawn(move || callback(dialog_fn()));
    }
}

pub(crate) fn pick_file(options: DialogOptions, on_completion: DialogCallback) -> Result<()> {
    run_dialog_then(
        move || dialog(options).pick_file().map(PickedFile::from_path),
        move |file| on_completion(Ok(file)),
    );
    Ok(())
}

pub(crate) fn save_data(
    options: DialogOptions,
    data: DialogData,
    on_completion: DialogCallback,
) -> Result<()> {
    let _ = options.output_file_name_only()?;
    run_dialog_then(
        move || dialog(options).save_file(),
        move |chosen| {
            let result = chosen.map(|path| {
                fs::write(&path, (*data).as_ref())?;
                Ok(PickedFile::from_path(path))
            });
            on_completion(result.transpose());
        },
    );
    Ok(())
}

pub(crate) fn pick_media(
    options: DialogOptions,
    media_kind: MediaKind,
    on_completion: DialogCallback,
) -> Result<()> {
    run_dialog_then(
        move || {
            media_dialog(options, media_kind)
                .pick_file()
                .map(PickedFile::from_path)
        },
        move |file| on_completion(Ok(file)),
    );
    Ok(())
}

pub(crate) fn save_to_downloads(
    options: DialogOptions,
    source_path: PathBuf,
    on_completion: DialogCallback,
) -> Result<()> {
    std::thread::spawn(move || {
        let result = save_to_downloads_inner(options, source_path).map(Some);
        on_completion(result);
    });
    Ok(())
}

fn save_to_downloads_inner(options: DialogOptions, source_path: PathBuf) -> Result<PickedFile> {
    let downloads = robius_directories::UserDirs::new()
        .and_then(|user_dirs| user_dirs.download_dir().map(Path::to_owned))
        .ok_or(Error::Unsupported)?;
    fs::create_dir_all(&downloads)?;

    let file_name = options.output_file_name(&source_path)?;
    let destination = unique_path(downloads.join(file_name));
    copy_file(&source_path, &destination)?;

    Ok(PickedFile::from_path(destination))
}

fn copy_file(source_path: &Path, destination: &Path) -> Result<()> {
    if source_path == destination {
        return Ok(());
    }
    fs::copy(source_path, destination)?;
    Ok(())
}

fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }

    let parent = path.parent().map(Path::to_owned).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("Untitled");
    let extension = path.extension().and_then(|extension| extension.to_str());

    for index in 1.. {
        let mut file_name = format!("{stem} ({index})");
        if let Some(extension) = extension {
            file_name.push('.');
            file_name.push_str(extension);
        }

        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!()
}

fn dialog(options: DialogOptions) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new();
    for filter in options.filters {
        dialog = dialog.add_filter(filter.name, &filter.extensions);
    }
    // An explicit directory wins; otherwise fall back to the well-known-folder hint.
    let initial_dir = options.directory.or_else(
        || options.start_location.and_then(resolve_start_location)
    );
    if let Some(directory) = initial_dir {
        dialog = dialog.set_directory(directory);
    }
    if let Some(file_name) = options.file_name {
        dialog = dialog.set_file_name(file_name);
    }
    if let Some(title) = options.title {
        dialog = dialog.set_title(title);
    }
    dialog
}

fn resolve_start_location(location: StartLocation) -> Option<PathBuf> {
    let user_dirs = robius_directories::UserDirs::new()?;
    let dir = match location {
        StartLocation::Documents => user_dirs.document_dir(),
        StartLocation::Downloads => user_dirs.download_dir(),
        StartLocation::Pictures  => user_dirs.picture_dir(),
        StartLocation::Music     => user_dirs.audio_dir(),
        StartLocation::Videos    => user_dirs.video_dir(),
        StartLocation::Desktop   => user_dirs.desktop_dir(),
    };
    dir.map(Path::to_owned)
}

fn media_dialog(options: DialogOptions, media_kind: MediaKind) -> rfd::FileDialog {
    let use_defaults = options.filters.is_empty();
    let mut dialog = dialog(options);

    if use_defaults {
        match media_kind {
            MediaKind::Image => {
                dialog = dialog.add_filter("Images", DEFAULT_IMAGE_EXTENSIONS);
            }
            MediaKind::Video => {
                dialog = dialog.add_filter("Videos", DEFAULT_VIDEO_EXTENSIONS);
            }
            MediaKind::ImageOrVideo => {
                dialog = dialog
                    .add_filter("Images", DEFAULT_IMAGE_EXTENSIONS)
                    .add_filter("Videos", DEFAULT_VIDEO_EXTENSIONS);
            }
        }
    }

    dialog
}
