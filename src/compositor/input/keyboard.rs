use compositor::Server;
use wlroots::{key_events::KeyEvent, xkbcommon::xkb::{KEY_Escape, KEY_F1}, CompositorHandle,
              KeyboardHandle, KeyboardHandler, WLR_KEY_PRESSED};
pub struct Keyboard;

impl KeyboardHandler for Keyboard {
    fn on_key(&mut self, compositor: CompositorHandle, keyboard: KeyboardHandle, event: &KeyEvent) {
        with_handles!([(compositor: {compositor})] => {
            if event.key_state() == WLR_KEY_PRESSED {
                for key in event.pressed_keys() {
                    if key == KEY_Escape {
                        compositor.terminate();
                        ::awesome::lua::terminate();
                        // TODO Remove
                    } else if key == KEY_F1 {
                        ::std::thread::spawn(|| {
                            ::std::process::Command::new("weston-terminal").output()
                                .unwrap()
                        });
                    }
                }
            }
            let server: &mut Server = compositor.into();
            with_handles!([(seat: {&mut server.seat.seat}),
                           (keyboard: {keyboard})] => {
                seat.keyboard_notify_key(event.time_msec(),
                                         event.keycode(),
                                         event.key_state() as u32);
                seat.keyboard_send_modifiers(&mut keyboard.get_modifier_masks());
            }).expect("Seat was destroyed");
        }).unwrap();
    }
}
