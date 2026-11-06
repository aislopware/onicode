//! Ask for microphone permission in the viewport's macOS process identity
//! before its engine opens native devices. CoreAudio can otherwise yield silence
//! without displaying a privacy prompt. The read and the alert are the
//! onboarding checklist's own (`onicode_grants::microphone`); starting voice is
//! the person's click, so it may raise the alert if onboarding was skipped.
use onicode_proto::voice::VoiceRejection;

pub(super) async fn microphone() -> Result<(), VoiceRejection> {
    #[cfg(target_os = "macos")]
    {
        use onicode_grants::{Status, microphone};
        match microphone::status() {
            Status::Granted => {}
            Status::Denied => return Err(VoiceRejection::MicrophonePermissionDenied),
            Status::NotAsked | Status::Off => {
                // Requesting capture permission without a purpose string terminates the
                // process. Both app bundles and direct Cargo builds supply this string.
                if !microphone::described() {
                    return Err(VoiceRejection::MicrophoneMetadataMissing);
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                if !microphone::request(move |granted| {
                    let _sent = tx.send(granted);
                }) {
                    return Err(VoiceRejection::DeviceUnavailable);
                }
                if !rx.await.map_err(|_| VoiceRejection::DeviceUnavailable)? {
                    return Err(VoiceRejection::MicrophonePermissionDenied);
                }
            }
            Status::Unknown | Status::Unavailable => {
                return Err(VoiceRejection::DeviceUnavailable);
            }
        }
    }
    Ok(())
}
