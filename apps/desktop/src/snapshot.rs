//! Dev only (`--features snapshot`): with `QUIPCALL_SNAPSHOT_DIR` set, every
//! window is rendered to a PNG there every two seconds. Lets you check layouts
//! without screen-recording access. AppKit effects (circle clipping, blur) are
//! not part of the image.

use std::time::Duration;

use gpui::App;

pub fn start(cx: &mut App) {
    let Ok(dir) = std::env::var("QUIPCALL_SNAPSHOT_DIR") else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            cx.update(|cx| {
                for handle in cx.windows() {
                    let id = handle.window_id().as_u64();
                    let _ = handle.update(cx, |_, window, _| {
                        let size = window.bounds().size;
                        let path = format!(
                            "{dir}/window-{id}-{}x{}.png",
                            (size.width / gpui::px(1.0)) as u32,
                            (size.height / gpui::px(1.0)) as u32
                        );
                        match window.render_to_image() {
                            Ok(image) => {
                                if let Err(e) = image.save(&path) {
                                    tracing::warn!("snapshot {path}: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("snapshot {path}: {e:#}"),
                        }
                    });
                }
            });
        }
    })
    .detach();
}
