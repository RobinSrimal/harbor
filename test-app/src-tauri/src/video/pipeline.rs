//! Video pipeline: camera capture → H.264 encode → MOQ publish.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use harbor_core::Protocol;

use super::capture::CameraCapture;
use super::encoder::H264Encoder;

pub struct VideoPipeline {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl VideoPipeline {
    /// Start capturing from camera, encoding to H.264, and publishing via MOQ.
    ///
    /// `request_id` must correspond to an active stream where `StreamConnected`
    /// has been received with `is_source: true`.
    pub async fn start(
        protocol: Arc<Mutex<Option<Protocol>>>,
        request_id: [u8; 32],
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<Self, String> {
        // Get the BroadcastProducer from the protocol
        let mut producer = {
            let guard = protocol.lock().await;
            let proto = guard.as_ref().ok_or("Protocol not running")?;
            proto
                .publish_to_stream(&request_id, "video")
                .await
                .map_err(|e| format!("Failed to get BroadcastProducer: {e}"))?
        };

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Start camera on its own thread
        let camera = CameraCapture::start(width, height, fps)?;
        eprintln!("[VIDEO-TX] Camera started, waiting for consumer to subscribe...");

        // Use a tokio task so that requested_track().await runs on the main runtime
        let handle = tokio::spawn(async move {
            // Wait for the consumer to subscribe to a track
            let mut track = tokio::select! {
                _ = cancel_clone.cancelled() => {
                    eprintln!("[VIDEO-TX] Cancelled while waiting for track request");
                    return;
                }
                track_opt = producer.requested_track() => {
                    match track_opt {
                        Some(t) => {
                            eprintln!("[VIDEO-TX] Consumer subscribed to track: {}", t.info.name);
                            t
                        }
                        None => {
                            eprintln!("[VIDEO-TX] No track requested (producer closed)");
                            return;
                        }
                    }
                }
            };

            eprintln!("[VIDEO-TX] Track ready, starting encode loop");

            let interval = Duration::from_secs_f64(1.0 / fps as f64);
            let mut frames_sent: u64 = 0;
            let mut encoder: Option<H264Encoder> = None;

            loop {
                if cancel_clone.is_cancelled() {
                    break;
                }

                // Pop latest frame (non-blocking)
                if let Some(frame) = camera.pop_frame() {
                    // Lazily create encoder from first frame's actual dimensions
                    let enc = match &mut encoder {
                        Some(e) => e,
                        None => {
                            eprintln!(
                                "[VIDEO-TX] First frame: {}x{} ({} bytes) @ {}fps",
                                frame.width, frame.height, frame.data.len(), fps
                            );
                            match H264Encoder::new(frame.width, frame.height, fps) {
                                Ok(e) => encoder.insert(e),
                                Err(e) => {
                                    eprintln!("[VIDEO-TX] Failed to create encoder: {e}");
                                    break;
                                }
                            }
                        }
                    };

                    // Encode
                    if let Err(e) = enc.push_frame(frame.data) {
                        eprintln!("[VIDEO-TX] Encode error: {e}");
                        continue;
                    }

                    // Collect and publish encoded packets
                    while let Some(mut hang_frame) = enc.pop_hang_frame() {
                        use bytes::Buf;
                        let payload_bytes =
                            hang_frame.payload.copy_to_bytes(hang_frame.payload.remaining());
                        track.write_frame(payload_bytes);
                        frames_sent += 1;
                        if frames_sent % (fps as u64) == 0 {
                            eprintln!("[VIDEO-TX] {} frames published", frames_sent);
                        }
                    }
                }

                // Yield to tokio + pace at target fps
                tokio::time::sleep(interval).await;
            }

            if let Some(enc) = &encoder {
                enc.flush();
            }
            eprintln!("[VIDEO-TX] Pipeline stopped after {frames_sent} frames");
        });

        Ok(Self {
            cancel,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }

    pub fn is_running(&self) -> bool {
        !self.cancel.is_cancelled()
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
