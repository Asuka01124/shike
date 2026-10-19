#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod input;
mod ui;
use fs2::FileExt;
use gpui::*;
fn main() {
    let demo = std::env::args().any(|s| s == "--demo");
    if std::env::args().any(|s| s == "--probe") {
        let result = shike::api::Api::new("").and_then(|a| a.probe_anonymous("6ERAcwloghvoka00"));
        let text = match result {
            Ok(v) => v.to_string(),
            Err(e) => format!("{e}"),
        };
        let _ = std::fs::write("rust-api-probe.json", &text);
        println!("{text}");
        return;
    }
    let dir = shike::store::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("app.lock"))
        .ok();
    if !demo {
        if let Some(file) = &lock {
            if file.try_lock_exclusive().is_err() {
                #[cfg(windows)]
                unsafe {
                    use windows::{Win32::UI::WindowsAndMessaging::FindWindowW, core::w};
                    if let Ok(h) = FindWindowW(None, w!("拾刻")) {
                        ui::show_native(h.0 as usize);
                    }
                }
                return;
            }
        }
    }
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(930.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(790.), px(610.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("拾刻".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| ui::Desk::new(window, cx, demo)),
        )
        .unwrap();
        cx.activate(true);
    });
    drop(lock);
}
