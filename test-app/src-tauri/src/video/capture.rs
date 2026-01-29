use std::sync::mpsc;
use std::time::{Duration, Instant};

use nokhwa::{
    pixel_format::RgbAFormat,
    utils::{RequestedFormat, RequestedFormatType, Resolution, ApiBackend},
};

pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA pixel data
    pub data: Vec<u8>,
}

pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

/// List available cameras.
pub fn list_cameras() -> Result<Vec<CameraInfo>, String> {
    let cameras = nokhwa::query(ApiBackend::Auto).map_err(|e| e.to_string())?;
    Ok(cameras
        .iter()
        .enumerate()
        .map(|(i, info)| CameraInfo {
            index: i as u32,
            name: info.human_name().to_string(),
        })
        .collect())
}

pub struct CameraCapture {
    shutdown_tx: Option<mpsc::Sender<()>>,
    frame_rx: mpsc::Receiver<CameraFrame>,
    handle: Option<std::thread::JoinHandle<()>>,
    pub width: u32,
    pub height: u32,
}

impl CameraCapture {
    /// Start camera capture. The camera is opened and used entirely within
    /// a dedicated thread to avoid Send issues with nokhwa's backend.
    pub fn start(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let (frame_tx, frame_rx) = mpsc::sync_channel(2);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        // Channel to report back the actual resolution (or error)
        let (init_tx, init_rx) = mpsc::channel::<Result<(u32, u32), String>>();

        let handle = std::thread::Builder::new()
            .name("camera-capture".into())
            .spawn(move || {
                nokhwa::nokhwa_initialize(|_granted| {});

                let cameras = match nokhwa::query(ApiBackend::Auto) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(format!("Failed to query cameras: {e}")));
                        return;
                    }
                };
                if cameras.is_empty() {
                    let _ = init_tx.send(Err("No cameras available".to_string()));
                    return;
                }

                let camera_index = cameras.first().unwrap().index().clone();
                let desired_res = Resolution::new(width, height);

                // Try different format types — AbsoluteHighestFrameRate is most
                // permissive and lets the backend pick whatever works.
                let mut camera = match nokhwa::Camera::new(
                    camera_index.clone(),
                    RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(
                        nokhwa::utils::CameraFormat::new(
                            desired_res,
                            nokhwa::utils::FrameFormat::RAWRGB,
                            fps,
                        ),
                    )),
                ) {
                    Ok(c) => c,
                    Err(_e1) => {
                        tracing::warn!("Closest format failed, trying AbsoluteHighestFrameRate: {_e1}");
                        match nokhwa::Camera::new(
                            camera_index,
                            RequestedFormat::new::<RgbAFormat>(
                                RequestedFormatType::AbsoluteHighestFrameRate,
                            ),
                        ) {
                            Ok(c) => c,
                            Err(e2) => {
                                let _ = init_tx.send(Err(format!("Failed to open camera: {e2}")));
                                return;
                            }
                        }
                    }
                };

                let actual_res = camera.resolution();
                let actual_w = actual_res.width();
                let actual_h = actual_res.height();

                if let Err(e) = camera.open_stream() {
                    let _ = init_tx.send(Err(format!("Failed to open stream: {e}")));
                    return;
                }

                // Report success + actual resolution
                let _ = init_tx.send(Ok((actual_w, actual_h)));

                let interval = Duration::from_secs_f64(1.0 / fps as f64);
                loop {
                    let start = Instant::now();
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }

                    match camera.frame() {
                        Ok(frame) => {
                            match frame.decode_image::<RgbAFormat>() {
                                Ok(image) => {
                                    let iw = image.width();
                                    let ih = image.height();
                                    let cf = CameraFrame {
                                        width: iw,
                                        height: ih,
                                        data: image.into_raw(),
                                    };
                                    let _ = frame_tx.try_send(cf);
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to decode frame: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to capture frame: {e}");
                        }
                    }

                    let elapsed = start.elapsed();
                    if elapsed < interval {
                        std::thread::sleep(interval - elapsed);
                    }
                }
                let _ = camera.stop_stream();
            })
            .map_err(|e| format!("Failed to spawn capture thread: {e}"))?;

        // Wait for camera initialization
        let (actual_w, actual_h) = init_rx
            .recv()
            .map_err(|_| "Camera thread died during init".to_string())?
            .map_err(|e| e)?;

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            frame_rx,
            handle: Some(handle),
            width: actual_w,
            height: actual_h,
        })
    }

    pub fn pop_frame(&self) -> Option<CameraFrame> {
        let mut latest = None;
        while let Ok(frame) = self.frame_rx.try_recv() {
            latest = Some(frame);
        }
        latest
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
