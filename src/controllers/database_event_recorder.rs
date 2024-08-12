use crate::Error;
use derive_more::Display;
use kube::runtime::events::{Event, EventType, Recorder};

pub struct DatabaseEventRecorder(Recorder);

impl Into<DatabaseEventRecorder> for Recorder {
    fn into(self) -> DatabaseEventRecorder {
        DatabaseEventRecorder(self)
    }
}

impl DatabaseEventRecorder {
    pub async fn info<N>(&self, action: EventAction, reason: EventReason, note: N) -> Result<(), Error>
    where
        N: Into<String>,
    {
        self.publish(EventType::Normal, action, reason, note).await
    }

    pub async fn warn<N>(&self, action: EventAction, reason: EventReason, note: N) -> Result<(), Error>
    where
        N: Into<String>,
    {
        self.publish(EventType::Warning, action, reason, note).await
    }

    pub async fn publish<N>(&self, type_: EventType, action: EventAction, reason: EventReason, note: N) -> Result<(), Error>
    where
        N: Into<String>,
    {
        Ok(self.0
            .publish(Event {
                type_,
                action: action.to_string(),
                reason: reason.to_string(),
                note: Some(note.into()),
                secondary: None,
            })
            .await?)
    }
}

#[derive(Display)]
pub enum EventAction {
    #[display("CreateDatabase")]
    CreateDatabase,
    #[display("ReconcileDatabase")]
    ReconcileDatabase,
    #[display("ApplyGrant")]
    ApplyGrant,
    #[display("ApplyComment")]
    ApplyComment,
    #[display("DeleteDatabase")]
    DeleteDatabase,
}

#[derive(Display)]
pub enum EventReason {
    #[display("MissingServer")]
    MissingServer,
    #[display("InspectionFailed")]
    InspectionFailed,
    #[display("InvalidName")]
    InvalidName,
    #[display("CreateRequested")]
    CreateRequested,
    #[display("Success")]
    Success,
    #[display("Failed")]
    Failed,
    #[display("Ignored")]
    Ignored,
}
