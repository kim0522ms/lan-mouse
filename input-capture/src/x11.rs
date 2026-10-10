use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    ptr,
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use futures_core::Stream;
use tokio::time::{Sleep, sleep};
use x11::xlib;

use super::{Capture, CaptureError, CaptureEvent, Position, error::X11InputCaptureCreationError};

const POLL_INTERVAL: Duration = Duration::from_millis(16);
const EDGE_PX: i32 = 2;

pub struct X11InputCapture {
    display: *mut xlib::Display,
    root: xlib::Window,
    active_positions: HashSet<Position>,
    active_edge: Option<Position>,
    poll_timer: Pin<Box<Sleep>>,
}

unsafe impl Send for X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> std::result::Result<Self, X11InputCaptureCreationError> {
        let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(X11InputCaptureCreationError::DisplayOpen);
        }

        let root = unsafe { xlib::XDefaultRootWindow(display) };
        Ok(Self {
            display,
            root,
            active_positions: HashSet::new(),
            active_edge: None,
            poll_timer: Box::pin(sleep(POLL_INTERVAL)),
        })
    }

    fn pointer_edge(&self) -> Option<(Position, Position)> {
        let mut attrs: xlib::XWindowAttributes = unsafe { std::mem::zeroed() };
        let got_attrs = unsafe { xlib::XGetWindowAttributes(self.display, self.root, &mut attrs) };
        if got_attrs == 0 {
            return None;
        }

        let mut root_return: xlib::Window = 0;
        let mut child_return: xlib::Window = 0;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask_return = 0;
        let found_pointer = unsafe {
            xlib::XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask_return,
            )
        };
        if found_pointer == 0 {
            return None;
        }

        let width = attrs.width;
        let height = attrs.height;
        let detected_edge = [
            (Position::Left, root_x <= EDGE_PX),
            (Position::Right, root_x >= width.saturating_sub(EDGE_PX + 1)),
            (Position::Top, root_y <= EDGE_PX),
            (
                Position::Bottom,
                root_y >= height.saturating_sub(EDGE_PX + 1),
            ),
        ]
        .into_iter()
        .find_map(|(pos, active)| active.then_some(pos))?;

        choose_capture_edge(&self.active_positions, detected_edge)
            .map(|capture_edge| (detected_edge, capture_edge))
    }
}

fn choose_capture_edge(
    active_positions: &HashSet<Position>,
    detected_edge: Position,
) -> Option<Position> {
    active_positions
        .contains(&detected_edge)
        .then_some(detected_edge)
}

impl Drop for X11InputCapture {
    fn drop(&mut self) {
        unsafe {
            xlib::XCloseDisplay(self.display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(positions: &[Position]) -> HashSet<Position> {
        positions.iter().copied().collect()
    }

    #[test]
    fn accepts_matching_edge() {
        let active = positions(&[Position::Right]);

        assert_eq!(
            choose_capture_edge(&active, Position::Right),
            Some(Position::Right)
        );
    }

    #[test]
    fn rejects_perpendicular_edge_for_single_return_barrier() {
        let active = positions(&[Position::Right]);

        assert_eq!(choose_capture_edge(&active, Position::Bottom), None);
        assert_eq!(choose_capture_edge(&active, Position::Top), None);
    }
}

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.active_positions.insert(pos);
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.active_positions.remove(&pos);
        if self.active_edge == Some(pos) {
            self.active_edge = None;
        }
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        // Keep the edge latched until the pointer actually leaves it. The
        // service releases enter-only barriers immediately after notifying
        // the peer, and clearing this here would retrigger every poll tick.
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for X11InputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.poll_timer.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        self.poll_timer = Box::pin(sleep(POLL_INTERVAL));
        let _ = self.poll_timer.as_mut().poll(cx);

        let edge = self.pointer_edge();
        if edge.is_none() {
            self.active_edge = None;
            return Poll::Pending;
        }

        let (detected_edge, capture_edge) = edge.expect("checked above");
        if Some(detected_edge) != self.active_edge {
            self.active_edge = Some(detected_edge);
            if detected_edge == capture_edge {
                log::info!("X11 edge detected at {detected_edge}");
            } else {
                log::info!(
                    "X11 edge detected at {detected_edge}; using {capture_edge} return barrier"
                );
            }
            return Poll::Ready(Some(Ok((capture_edge, CaptureEvent::Begin))));
        }

        Poll::Pending
    }
}
