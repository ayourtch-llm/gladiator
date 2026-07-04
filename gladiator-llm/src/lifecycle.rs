use crate::event::LlmEvent;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LifecycleState {
    #[default]
    Idle,
    Reasoning,
    Text,
}

#[derive(Debug, Default)]
pub struct StreamLifecycle {
    pub state: LifecycleState,
    pub reasoning_id: String,
    pub text_id: String,
}

impl StreamLifecycle {
    pub fn new() -> Self {
        Self {
            state: LifecycleState::Idle,
            reasoning_id: "reasoning-0".to_string(),
            text_id: "text-0".to_string(),
        }
    }

    pub fn reasoning_delta(&mut self, events: &mut Vec<LlmEvent>, text: &str) {
        if self.state != LifecycleState::Reasoning {
            self.state = LifecycleState::Reasoning;
            events.push(LlmEvent::ReasoningStart {
                id: self.reasoning_id.clone(),
            });
        }
        events.push(LlmEvent::ReasoningDelta {
            id: self.reasoning_id.clone(),
            text: text.to_string(),
        });
    }

    pub fn reasoning_end(&mut self, events: &mut Vec<LlmEvent>) {
        if self.state == LifecycleState::Reasoning {
            events.push(LlmEvent::ReasoningEnd {
                id: self.reasoning_id.clone(),
            });
            self.state = LifecycleState::Idle;
        }
    }

    pub fn text_delta(&mut self, events: &mut Vec<LlmEvent>, text: &str) {
        self.reasoning_end(events);
        if self.state != LifecycleState::Text {
            self.state = LifecycleState::Text;
            events.push(LlmEvent::TextStart {
                id: self.text_id.clone(),
            });
        }
        events.push(LlmEvent::TextDelta {
            id: self.text_id.clone(),
            text: text.to_string(),
        });
    }

    pub fn text_end(&mut self, events: &mut Vec<LlmEvent>) {
        if self.state == LifecycleState::Text {
            events.push(LlmEvent::TextEnd {
                id: self.text_id.clone(),
            });
            self.state = LifecycleState::Idle;
        }
    }

    pub fn finish(&mut self, events: &mut Vec<LlmEvent>, reason: &str) {
        self.reasoning_end(events);
        self.text_end(events);
        events.push(LlmEvent::Finish {
            reason: reason.to_string(),
            usage: None,
        });
    }
}
