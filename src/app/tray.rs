//! Icon in the notification area: the window can be hidden there and comes
//! back from its menu, a click on the icon, or the Scan button of the mouse.

use crossbeam_channel::{Receiver, unbounded};
use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrayCmd {
    Show,
    Scan,
    Quit,
}

pub(super) struct Tray {
    _icon: TrayIcon,
    pub(super) rx: Receiver<TrayCmd>,
}

impl Tray {
    /// The menu and icon events arrive on the windowing thread, possibly while
    /// the window is hidden: they are queued and the application is woken up.
    pub(super) fn new(ctx: &egui::Context) -> anyhow::Result<Self> {
        let show = MenuItem::new("Ouvrir Mulot-scan", true, None);
        let scan = MenuItem::new("Scanner", true, None);
        let quit = MenuItem::new("Quitter", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show, &scan, &PredefinedMenuItem::separator(), &quit])?;
        let ids = [
            (show.id().clone(), TrayCmd::Show),
            (scan.id().clone(), TrayCmd::Scan),
            (quit.id().clone(), TrayCmd::Quit),
        ];

        let (tx, rx) = unbounded();
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some((_, cmd)) = ids.iter().find(|(id, _)| *id == e.id) {
                let _ = menu_tx.send(*cmd);
                menu_ctx.request_repaint();
            }
        }));
        let icon_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let open = matches!(
                e,
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                    | TrayIconEvent::DoubleClick { .. }
            );
            if open {
                let _ = tx.send(TrayCmd::Show);
                icon_ctx.request_repaint();
            }
        }));

        let img =
            image::load_from_memory_with_format(include_bytes!("../../assets/icon-256.png"), image::ImageFormat::Png)?;
        let img = img.resize(64, 64, image::imageops::FilterType::Triangle).to_rgba8();
        let (w, h) = img.dimensions();
        let icon = TrayIconBuilder::new()
            .with_id("mulot-scan")
            .with_title("Mulot-scan")
            .with_tooltip("Mulot-scan — appuyez sur le bouton Scan de la souris pour scanner")
            .with_icon(Icon::from_rgba(img.into_raw(), w, h)?)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self { _icon: icon, rx })
    }
}
