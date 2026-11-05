//! Windows notification-area icon. Closing the window only hides Jcode, like
//! other chat apps on Windows, so the tray is how people find it again and
//! how they quit for real.

use anyhow::Context;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    Quit,
}

fn icon() -> anyhow::Result<Icon> {
    let image = image::load_from_memory(include_bytes!("../../assets/tray-icon.png"))
        .context("decode tray icon")?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).context("create tray icon")
}

pub fn install(
    cx: &mut gpui::App,
    mut dispatch: impl FnMut(TrayAction, &mut gpui::App) + 'static,
) -> anyhow::Result<()> {
    // The icon disappears when this is dropped, so it lives as long as the app.
    struct Tray {
        _icon: TrayIcon,
    }
    impl gpui::Global for Tray {}

    let open = MenuItem::new("Open Jcode", true, None);
    let quit = MenuItem::new("Quit Jcode", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .context("build tray menu")?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon()?)
        .with_tooltip("Jcode")
        .with_menu(Box::new(menu))
        // Left click opens the window; the menu stays on right click.
        .with_menu_on_left_click(false)
        .build()
        .context("create tray icon")?;

    let (sender, receiver) = async_channel::unbounded();
    TrayIconEvent::set_event_handler(Some({
        let sender = sender.clone();
        move |event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = sender.try_send(TrayAction::Open);
            }
        }
    }));
    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id == open_id {
            TrayAction::Open
        } else if event.id == quit_id {
            TrayAction::Quit
        } else {
            return;
        };
        let _ = sender.try_send(action);
    }));

    cx.set_global(Tray { _icon: tray });
    cx.spawn(async move |cx| {
        while let Ok(action) = receiver.recv().await {
            // Handlers run inside the tray's window procedure; act on GPUI's turn.
            let _ = cx.update(|cx| dispatch(action, cx));
        }
    })
    .detach();
    Ok(())
}
