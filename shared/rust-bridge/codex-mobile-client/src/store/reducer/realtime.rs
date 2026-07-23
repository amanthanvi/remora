use crate::session::events::UiEvent;
use crate::store::snapshot::AppVoiceSessionSnapshot;
use crate::store::updates::AppStoreUpdateRecord;
use crate::store::voice::VoiceDerivedUpdate;
use crate::types::{
    AppVoiceSessionPhase, AppVoiceTranscriptEntry, AppVoiceTranscriptUpdate, ThreadKey,
};

use super::AppStoreReducer;

impl AppStoreReducer {
    pub(super) fn apply_realtime_event(&self, event: &UiEvent) {
        match event {
            UiEvent::RealtimeStarted { key, notification } => {
                self.voice_state.reset_thread(key);
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot.voice_session.active_thread = Some(key.clone());
                    snapshot.voice_session.session_id = notification.realtime_session_id.clone();
                    snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Listening);
                    snapshot.voice_session.last_error = None;
                    snapshot.voice_session.transcript_entries.clear();
                    snapshot.voice_session.handoff_thread_key = None;
                    if let Some(thread) = snapshot.threads.get_mut(key) {
                        thread.realtime_session_id = notification.realtime_session_id.clone();
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeStartedNotification {
                    thread_id: notification.thread_id.clone(),
                    session_id: notification.realtime_session_id.clone(),
                    version: match notification.version {
                        codex_protocol::protocol::RealtimeConversationVersion::V1 => {
                            "v1".to_string()
                        }
                        codex_protocol::protocol::RealtimeConversationVersion::V2 => {
                            "v2".to_string()
                        }
                    },
                };
                self.emit(AppStoreUpdateRecord::RealtimeStarted {
                    key: key.clone(),
                    notification: protocol_notification,
                });
                self.emit_thread_metadata_changed(key);
            }
            UiEvent::RealtimeSdp { key, notification } => {
                let protocol_notification =
                    crate::types::AppRealtimeSdpNotification::from(notification.clone());
                self.emit(AppStoreUpdateRecord::RealtimeSdp {
                    key: key.clone(),
                    notification: protocol_notification,
                });
            }
            UiEvent::RealtimeTranscriptUpdated { key, role, text } => {
                for update in self
                    .voice_state
                    .handle_typed_transcript_delta(key, role, text)
                {
                    if let VoiceDerivedUpdate::Transcript(update) = update {
                        self.apply_voice_transcript_update(key, &update);
                        self.emit(AppStoreUpdateRecord::RealtimeTranscriptUpdated {
                            key: key.clone(),
                            update,
                        });
                    }
                }
            }
            UiEvent::RealtimeItemAdded { key, notification } => {
                for update in self.voice_state.handle_item(key, &notification.item) {
                    match update {
                        VoiceDerivedUpdate::Transcript(update) => {
                            self.apply_voice_transcript_update(key, &update);
                            self.emit(AppStoreUpdateRecord::RealtimeTranscriptUpdated {
                                key: key.clone(),
                                update,
                            });
                        }
                        VoiceDerivedUpdate::HandoffRequest(request) => {
                            {
                                let mut snapshot =
                                    self.snapshot.write().expect("app store lock poisoned");
                                snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Handoff);
                            }
                            self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                            self.emit(AppStoreUpdateRecord::RealtimeHandoffRequested {
                                key: key.clone(),
                                request,
                            });
                        }
                        VoiceDerivedUpdate::SpeechStarted => {
                            {
                                let mut snapshot =
                                    self.snapshot.write().expect("app store lock poisoned");
                                snapshot.voice_session.phase =
                                    Some(AppVoiceSessionPhase::Listening);
                            }
                            self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                            self.emit(AppStoreUpdateRecord::RealtimeSpeechStarted {
                                key: key.clone(),
                            });
                        }
                    }
                }
            }
            UiEvent::RealtimeOutputAudioDelta { key, notification } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if snapshot.voice_session.active_thread.as_ref() == Some(key) {
                        snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Speaking);
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeOutputAudioDeltaNotification {
                    thread_id: notification.thread_id.clone(),
                    audio: crate::types::AppRealtimeAudioChunk {
                        item_id: notification.audio.item_id.clone(),
                        data: notification.audio.data.clone(),
                        sample_rate: notification.audio.sample_rate,
                        num_channels: notification.audio.num_channels.into(),
                        samples_per_channel: notification.audio.samples_per_channel,
                    },
                };
                self.emit(AppStoreUpdateRecord::RealtimeOutputAudioDelta {
                    key: key.clone(),
                    notification: protocol_notification,
                });
            }
            UiEvent::RealtimeError { key, notification } => {
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Error);
                    snapshot.voice_session.last_error = Some(notification.message.clone());
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeErrorNotification {
                    thread_id: notification.thread_id.clone(),
                    message: notification.message.clone(),
                };
                self.emit(AppStoreUpdateRecord::RealtimeError {
                    key: key.clone(),
                    notification: protocol_notification,
                });
            }
            UiEvent::RealtimeClosed { key, notification } => {
                self.voice_state.clear_thread(key);
                {
                    let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
                    if let Some(thread) = snapshot.threads.get_mut(key) {
                        thread.realtime_session_id = None;
                    }
                    let reason = notification.reason.as_deref().unwrap_or("").trim();
                    if reason.is_empty() || reason == "requested" {
                        snapshot.voice_session = AppVoiceSessionSnapshot::default();
                    } else {
                        snapshot.voice_session.active_thread = Some(key.clone());
                        snapshot.voice_session.session_id = None;
                        snapshot.voice_session.phase = Some(AppVoiceSessionPhase::Error);
                        snapshot.voice_session.last_error = Some(reason.to_string());
                        snapshot.voice_session.handoff_thread_key = None;
                    }
                }
                self.emit(AppStoreUpdateRecord::VoiceSessionChanged);
                let protocol_notification = crate::types::AppRealtimeClosedNotification {
                    thread_id: notification.thread_id.clone(),
                    reason: notification.reason.clone(),
                };
                self.emit(AppStoreUpdateRecord::RealtimeClosed {
                    key: key.clone(),
                    notification: protocol_notification,
                });
                self.emit_thread_metadata_changed(key);
            }
            _ => unreachable!("non-realtime event delegated to realtime reducer"),
        }
    }

    fn apply_voice_transcript_update(&self, key: &ThreadKey, update: &AppVoiceTranscriptUpdate) {
        let mut snapshot = self.snapshot.write().expect("app store lock poisoned");
        if snapshot.voice_session.active_thread.as_ref() != Some(key) {
            return;
        }

        let entry = AppVoiceTranscriptEntry {
            item_id: update.item_id.clone(),
            speaker: update.speaker,
            text: update.text.clone(),
        };
        if let Some(existing) = snapshot
            .voice_session
            .transcript_entries
            .iter_mut()
            .find(|existing| existing.item_id == entry.item_id)
        {
            *existing = entry;
        } else {
            snapshot.voice_session.transcript_entries.push(entry);
        }

        snapshot.voice_session.phase = Some(match (update.speaker, update.is_final) {
            (_, false) => match update.speaker {
                crate::types::AppVoiceSpeaker::User => AppVoiceSessionPhase::Listening,
                crate::types::AppVoiceSpeaker::Assistant => AppVoiceSessionPhase::Speaking,
            },
            (crate::types::AppVoiceSpeaker::Assistant, true) => AppVoiceSessionPhase::Thinking,
            (crate::types::AppVoiceSpeaker::User, true) => AppVoiceSessionPhase::Listening,
        });
    }
}
