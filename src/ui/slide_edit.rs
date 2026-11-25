//! The slide window's share of the pen, and where things dropped between
//! the windows land. The window itself runs the app's own frame
//! (`app::view_pass`), on a slide or on the pins board.
//!
//! The pen reaches the app through one Wintab queue, in screen pixels.
//! A gesture that starts over the window is taken out of the canvas's
//! events (`claim_pen`) and handed to the window's frame in its own
//! client points (`take_pen_events`).

use crate::canvas::stroke::PenSample;
use crate::input::pen::PenEvent;

/// The pen gestures that belong to the slide window.
#[derive(Default)]
pub struct WindowPen {
    /// Pen events taken for the window this frame, in screen pixels.
    queue: Vec<PenEvent>,
    /// A pen gesture started over the window: its moves and its lift
    /// come here too, wherever the pen goes.
    claimed: bool,
}

fn sample_of(ev: &PenEvent) -> PenSample {
    match ev {
        PenEvent::Down(s) | PenEvent::Move(s) | PenEvent::Up(s) => *s,
    }
}

fn with_sample(ev: PenEvent, s: PenSample) -> PenEvent {
    match ev {
        PenEvent::Down(_) => PenEvent::Down(s),
        PenEvent::Move(_) => PenEvent::Move(s),
        PenEvent::Up(_) => PenEvent::Up(s),
    }
}

/// Where objects dragged from one window and let go over the other land:
/// the point grabbed (`start`, source slide units) under the pointer
/// (`target`, target slide units), at the same size on screen — `k` is
/// the source's screen pixels per unit over the target's.
pub fn drop_mapping(start: [f32; 2], target: [f32; 2], k: f32) -> crate::canvas::affine::Affine {
    use crate::canvas::affine::Affine;
    Affine::translate([-start[0], -start[1]]).then(Affine::scale_about([0.0, 0.0], k, k)).then(Affine::translate(target))
}

impl WindowPen {
    /// Take out of `events` (the canvas's pen events, in its window-client
    /// points from `client_origin`) every one of a gesture that started
    /// over the window `hwnd`, for the window's frame.
    pub fn claim(&mut self, events: &mut Vec<PenEvent>, client_origin: [i32; 2], hwnd: isize) {
        if hwnd == 0 {
            return;
        }
        let mut queue = std::mem::take(&mut self.queue);
        let mut claimed = self.claimed;
        events.retain(|ev| {
            let s = sample_of(ev);
            let screen = [client_origin[0] as f32 + s.pos[0], client_origin[1] as f32 + s.pos[1]];
            let mine = match ev {
                PenEvent::Down(_) => {
                    claimed = crate::platform::top_level_window_at(screen[0] as i32, screen[1] as i32) == hwnd;
                    claimed
                }
                PenEvent::Move(_) => claimed,
                PenEvent::Up(_) => std::mem::replace(&mut claimed, false),
            };
            if mine {
                queue.push(with_sample(*ev, PenSample { pos: screen, ..s }));
            }
            !mine
        });
        self.claimed = claimed;
        self.queue = queue;
    }

    /// The pen events taken for the window (`claim`), in its client
    /// points, and its client area's top-left on screen: what the app's
    /// frame for the window reads, as the overlay's reads its own.
    pub fn take(&mut self, hwnd: isize, ppp: f32) -> (Vec<PenEvent>, [i32; 2]) {
        let queue = std::mem::take(&mut self.queue);
        let Some((l, t, _, _)) = crate::platform::client_rect(hwnd) else { return (Vec::new(), [0, 0]) };
        let ppp = ppp.max(1e-3);
        let events = queue
            .into_iter()
            .map(|ev| {
                let s = sample_of(&ev);
                with_sample(ev, PenSample { pos: [(s.pos[0] - l as f32) / ppp, (s.pos[1] - t as f32) / ppp], ..s })
            })
            .collect();
        (events, [l, t])
    }

    /// A pen gesture over the window is under way.
    pub fn busy(&self) -> bool {
        self.claimed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_lands_under_the_pointer_at_the_same_size_on_screen() {
        // Grabbed at slide point (10, 20), let go over target point
        // (100, 50); the target shows things twice as big per unit.
        let m = drop_mapping([10.0, 20.0], [100.0, 50.0], 0.5);
        assert_eq!(m.apply([10.0, 20.0]), [100.0, 50.0], "the grabbed point is under the pointer");
        let p = m.apply([30.0, 20.0]);
        assert!((p[0] - 110.0).abs() < 1e-4 && (p[1] - 50.0).abs() < 1e-4, "20 units away become 10: {p:?}");
    }

    #[test]
    fn a_pin_seen_in_the_window_maps_back() {
        let pin = crate::canvas::pins::Pin { id: 1, center: [300.0, 200.0], scale: 2.0, angle: 0.5, local: false };
        let v = pin.viewed([40.0, -10.0], 0.25);
        assert_eq!((v.center, v.scale, v.angle), ([115.0, 40.0], 0.5, 0.5));
        let back = v.unviewed([40.0, -10.0], 0.25);
        assert!((back.center[0] - 300.0).abs() < 1e-3 && (back.center[1] - 200.0).abs() < 1e-3 && (back.scale - 2.0).abs() < 1e-5);
    }
}
