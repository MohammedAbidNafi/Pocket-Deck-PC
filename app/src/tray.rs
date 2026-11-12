//! The notification-area icon: click to open PocketDeck, right-click for the menu.

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
}

pub enum Action {
    Open,
    Quit,
}

impl Tray {
    /// Must be called on the UI thread. `on_action` is called on the UI thread too.
    pub fn new(on_action: fn(Action)) -> Result<Tray, String> {
        let open = MenuItem::with_id("open", "Open PocketDeck", true, None);
        let status = MenuItem::with_id("status", "Checking Sunshine…", false, None);
        let quit = MenuItem::with_id("quit", "Quit PocketDeck", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &status, &PredefinedMenuItem::separator(), &quit])
            .map_err(|e| e.to_string())?;

        // SAFETY: GetSystemMetrics has no preconditions.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
        let image = Icon::from_resource(1, Some((size, size))).map_err(|e| e.to_string())?;

        let icon = TrayIconBuilder::new()
            .with_icon(image)
            .with_tooltip("PocketDeck")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                "open" => Action::Open,
                "quit" => Action::Quit,
                _ => return,
            };
            let _ = slint::invoke_from_event_loop(move || on_action(action));
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                let _ = slint::invoke_from_event_loop(move || on_action(Action::Open));
            }
        }));

        Ok(Tray { icon, status })
    }

    pub fn set_status(&self, text: &str) {
        self.status.set_text(text);
        let _ = self.icon.set_tooltip(Some(format!("PocketDeck: {text}")));
    }
}
