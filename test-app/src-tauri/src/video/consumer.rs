//! Video consumer pipeline: MOQ consume → H.264 decode → JPEG frames for frontend.

use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use harbor_core::Protocol;

use super::decoder::H264Decoder;

pub struct VideoConsumer {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
    /// Latest JPEG frame as base64 string. Polled by the frontend.
    frame_rx: watch::Receiver<Option<String>>,
}

impl VideoConsumer {
    /// Start consuming video from an active MOQ stream session.
    ///
    /// `request_id` must correspond to a stream where `StreamConnected`
    /// has been received (typically with `is_source: false`).
    pub async fn start(
        protocol: Arc<Mutex<Option<Protocol>>>,
        request_id: [u8; 32],
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let (frame_tx, frame_rx) = watch::channel(None);

        // Get the BroadcastConsumer with retries
        let broadcast = {
            let mut result = None;
            for attempt in 0..20 {
                let guard = protocol.lock().await;
                if let Some(proto) = guard.as_ref() {
                    match proto.consume_stream(&request_id, "video").await {
                        Ok(b) => {
                            result = Some(b);
                            break;
                        }
                        Err(e) => {
                            if attempt >= 19 {
                                return Err(format!("Failed to consume stream after retries: {e}"));
                            }
                            drop(guard);
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                    }
                } else {
                    drop(guard);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            result.ok_or_else(|| "Failed to get broadcast consumer".to_string())?
        };

        eprintln!("[VIDEO-RX] Consumer starting, got broadcast consumer");

        let handle = tokio::spawn(async move {
            eprintln!("[VIDEO-RX] Subscribing to video track...");

            let mut track = broadcast.subscribe_track(&hang::moq_lite::Track {
                name: "video".to_string(),
                priority: 0,
            });

            eprintln!("[VIDEO-RX] Subscribed, waiting for groups...");

            let mut decoder = H264Decoder::new(width, height);
            let mut frames_received: u64 = 0;

            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    group_result = track.next_group() => {
                        match group_result {
                            Ok(Some(mut group)) => {
                                let mut group_frames = 0u32;
                                loop {
                                    tokio::select! {
                                        _ = cancel_clone.cancelled() => break,
                                        frame_result = group.read_frame() => {
                                            match frame_result {
                                                Ok(Some(frame_data)) => {
                                                    frames_received += 1;
                                                    group_frames += 1;

                                                    if frames_received <= 5 || frames_received % 30 == 0 {
                                                        eprintln!(
                                                            "[VIDEO-RX] packet #{} received, {} bytes",
                                                            frames_received, frame_data.len()
                                                        );
                                                    }

                                                    // Decode H.264
                                                    if let Err(e) = decoder.push_packet(&frame_data) {
                                                        eprintln!("[VIDEO-RX] Decoder push error: {e}");
                                                        continue;
                                                    }

                                                    // Drain decoded frames
                                                    let mut decoded_count = 0u32;
                                                    while let Some(decoded) = decoder.pop_frame() {
                                                        decoded_count += 1;
                                                        // Convert RGBA to JPEG then base64
                                                        match rgba_to_jpeg_base64(
                                                            &decoded.rgba_data,
                                                            decoded.width,
                                                            decoded.height,
                                                        ) {
                                                            Ok(b64) => {
                                                                if frames_received <= 5 {
                                                                    eprintln!("[VIDEO-RX] decoded {}x{}, JPEG b64 len={}", decoded.width, decoded.height, b64.len());
                                                                }
                                                                let _ = frame_tx.send(Some(b64));
                                                            }
                                                            Err(e) => {
                                                                eprintln!("[VIDEO-RX] JPEG encode error: {e}");
                                                            }
                                                        }
                                                    }
                                                    if frames_received <= 5 && decoded_count == 0 {
                                                        eprintln!("[VIDEO-RX] packet #{} produced 0 decoded frames", frames_received);
                                                    }
                                                }
                                                Ok(None) => {
                                                    if group_frames == 0 {
                                                        eprintln!("[VIDEO-RX] Empty group (0 frames)");
                                                    }
                                                    break;
                                                }
                                                Err(e) => {
                                                    tracing::warn!("read_frame error: {e}");
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                tracing::info!("Video track ended");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!("next_group error: {e}");
                                break;
                            }
                        }
                    }
                }
            }

            decoder.flush();
            tracing::info!("Video consumer stopped after {frames_received} packets");
        });

        Ok(Self {
            cancel,
            handle: Some(handle),
            frame_rx,
        })
    }

    /// Get the latest decoded frame as a base64-encoded JPEG string.
    pub fn get_frame(&self) -> Option<String> {
        self.frame_rx.borrow().clone()
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

impl Drop for VideoConsumer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Convert RGBA pixel data to a base64-encoded JPEG string.
fn rgba_to_jpeg_base64(rgba: &[u8], width: u32, height: u32) -> Result<String, String> {
    use image::{ImageBuffer, RgbImage, RgbaImage};
    use std::io::Cursor;

    let rgba_img: RgbaImage = ImageBuffer::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| "Failed to create image buffer".to_string())?;

    // Convert RGBA → RGB (JPEG doesn't support alpha)
    let rgb_img: RgbImage = ImageBuffer::from_fn(width, height, |x, y| {
        let p = rgba_img.get_pixel(x, y);
        image::Rgb([p[0], p[1], p[2]])
    });

    let mut buf = Cursor::new(Vec::new());
    rgb_img
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .map_err(|e| format!("JPEG encode failed: {e}"))?;

    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        buf.into_inner(),
    ))
}
