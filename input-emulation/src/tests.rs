use super::*;
use std::sync::{Arc, Mutex};

type RecordedEvents = Arc<Mutex<Vec<(EmulationHandle, Event)>>>;

struct RecordingEmulation(RecordedEvents);

#[async_trait]
impl Emulation for RecordingEmulation {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        self.0.lock().unwrap().push((handle, event));
        Ok(())
    }

    async fn create(&mut self, _: EmulationHandle) {}
    async fn destroy(&mut self, _: EmulationHandle) {}
    async fn terminate(&mut self) {}
}

#[tokio::test]
async fn disconnect_and_termination_release_keys_and_modifiers() {
    for terminate in [false, true] {
        let events = RecordedEvents::default();
        let mut input = InputEmulation {
            emulation: Box::new(RecordingEmulation(events.clone())),
            handles: Default::default(),
            pressed_keys: Default::default(),
        };
        assert!(input.create(7).await);
        let down = Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: 30,
            state: 1,
        });
        input.consume(down, 7).await.unwrap();
        input.consume(down, 7).await.unwrap();
        assert!(input.has_pressed_keys(7));
        assert_eq!(
            events.lock().unwrap().len(),
            1,
            "duplicate key-down is suppressed"
        );

        if terminate {
            input.terminate().await;
        } else {
            input.destroy(7).await;
        }
        assert!(!input.has_pressed_keys(7));
        assert!(input.handles.is_empty());
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                (7, down),
                (
                    7,
                    Event::Keyboard(KeyboardEvent::Key {
                        time: 0,
                        key: 30,
                        state: 0
                    })
                ),
                (
                    7,
                    Event::Keyboard(KeyboardEvent::Modifiers {
                        depressed: 0,
                        latched: 0,
                        locked: 0,
                        group: 0,
                    })
                ),
            ]
        );
    }
}
