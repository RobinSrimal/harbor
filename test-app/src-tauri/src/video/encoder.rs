//! H.264 hardware encoder using Apple VideoToolbox via raw FFI.
//!
//! Uses VTCompressionSession to encode BGRA frames to H.264 NAL units
//! with hardware acceleration.

use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc;

use bytes::Bytes;
use hang::Timestamp;

// CoreFoundation types
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFTypeRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFIndex = isize;

// CoreMedia types
type CMSampleBufferRef = *const c_void;
type CMFormatDescriptionRef = *const c_void;
type CMBlockBufferRef = *const c_void;

// CoreVideo types
type CVPixelBufferRef = *mut c_void;
type CVReturn = i32;

// VideoToolbox types
type VTCompressionSessionRef = *mut c_void;
type VTEncodeInfoFlags = u32;
type OSStatus = i32;

#[repr(C)]
#[derive(Copy, Clone)]
struct CMTimeStruct {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTimeStruct {
    fn make(value: i64, timescale: i32) -> Self {
        Self {
            value,
            timescale,
            flags: 1, // kCMTimeFlags_Valid
            epoch: 0,
        }
    }

    fn invalid() -> Self {
        Self {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        }
    }
}

// FourCC constants
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = u32::from_be_bytes(*b"avc1");
const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = u32::from_be_bytes(*b"BGRA");

const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    static kCFBooleanTrue: CFBooleanRef;
    static kCFBooleanFalse: CFBooleanRef;

    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(
        dict: CFMutableDictionaryRef,
        key: *const c_void,
        value: *const c_void,
    );
    fn CFRelease(cf: *const c_void);
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: CFIndex,
        value_ptr: *const c_void,
    ) -> CFNumberRef;

    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: CMSampleBufferRef) -> CMTimeStruct;
    fn CMSampleBufferIsValid(sbuf: CMSampleBufferRef) -> u8;
    fn CMBlockBufferGetDataLength(buf: CMBlockBufferRef) -> usize;
    fn CMBlockBufferCopyDataBytes(
        buf: CMBlockBufferRef,
        offset: usize,
        length: usize,
        destination: *mut u8,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;

    fn CVPixelBufferCreateWithBytes(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        base_address: *mut c_void,
        bytes_per_row: usize,
        release_callback: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
        release_ref_con: *mut c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> CVReturn;
    fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;

    fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: Option<
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                OSStatus,
                VTEncodeInfoFlags,
                CMSampleBufferRef,
            ),
        >,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;

    fn VTSessionSetProperty(
        session: VTCompressionSessionRef,
        key: CFStringRef,
        value: CFTypeRef,
    ) -> OSStatus;

    fn VTCompressionSessionPrepareToEncodeFrames(
        session: VTCompressionSessionRef,
    ) -> OSStatus;

    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        presentation_time_stamp: CMTimeStruct,
        duration: CMTimeStruct,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut VTEncodeInfoFlags,
    ) -> OSStatus;

    fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_time_stamp: CMTimeStruct,
    ) -> OSStatus;

    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
}

/// Encoded H.264 packet ready to be sent via MOQ
pub struct EncodedPacket {
    pub data: Bytes,
    pub timestamp: Timestamp,
    pub keyframe: bool,
}

/// Context passed to the VTCompressionSession output callback
struct CallbackContext {
    packet_tx: mpsc::Sender<EncodedPacket>,
}

pub struct H264Encoder {
    session: VTCompressionSessionRef,
    width: u32,
    height: u32,
    frame_count: u64,
    timescale: i32,
    packet_rx: mpsc::Receiver<EncodedPacket>,
    // Box to prevent moving; raw pointer is given to callback
    _callback_ctx: Box<CallbackContext>,
}

// VTCompressionSession is thread-safe
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let (packet_tx, packet_rx) = mpsc::channel();
        let timescale = fps as i32 * 1000; // high-precision timescale

        let callback_ctx = Box::new(CallbackContext {
            packet_tx,
        });
        let ctx_ptr = &*callback_ctx as *const CallbackContext as *mut c_void;

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        unsafe {
            // Create pixel buffer attributes dict
            let pb_attrs = create_pixel_buffer_attrs(width, height);

            let status = VTCompressionSessionCreate(
                kCFAllocatorDefault,
                width as i32,
                height as i32,
                K_CM_VIDEO_CODEC_TYPE_H264,
                ptr::null(), // encoder_specification
                pb_attrs,
                kCFAllocatorDefault,
                Some(compression_output_callback),
                ctx_ptr,
                &mut session,
            );

            if !pb_attrs.is_null() {
                CFRelease(pb_attrs as *const c_void);
            }

            if status != 0 {
                return Err(format!("VTCompressionSessionCreate failed: {status}"));
            }

            // Configure session properties
            configure_session(session, fps)?;

            let status = VTCompressionSessionPrepareToEncodeFrames(session);
            if status != 0 {
                VTCompressionSessionInvalidate(session);
                return Err(format!("PrepareToEncodeFrames failed: {status}"));
            }
        }

        tracing::info!("H264Encoder initialized: {width}x{height} @ {fps}fps (VideoToolbox hardware)");

        Ok(Self {
            session,
            width,
            height,
            frame_count: 0,
            timescale,
            packet_rx,
            _callback_ctx: callback_ctx,
        })
    }

    /// Push an RGBA frame to the encoder.
    /// Accepts frames with stride padding (data may be larger than width*height*4).
    pub fn push_frame(&mut self, mut rgba_data: Vec<u8>) -> Result<(), String> {
        let min_len = (self.width * self.height * 4) as usize;
        if rgba_data.len() < min_len {
            return Err(format!(
                "Frame too small: got {} expected at least {}",
                rgba_data.len(),
                min_len
            ));
        }

        // Compute actual bytes-per-row (stride) from the data
        let bytes_per_row = rgba_data.len() / self.height as usize;

        // Convert RGBA → BGRA in-place (swap R and B), respecting stride
        let row_pixels = self.width as usize;
        for row in 0..self.height as usize {
            let row_start = row * bytes_per_row;
            for px in 0..row_pixels {
                let i = row_start + px * 4;
                rgba_data.swap(i, i + 2);
            }
        }

        let pts = CMTimeStruct::make(self.frame_count as i64, self.timescale);
        let duration = CMTimeStruct::make(1, self.timescale / 1000); // 1/fps seconds

        unsafe {
            let mut pixel_buffer: CVPixelBufferRef = ptr::null_mut();

            let status = CVPixelBufferCreateWithBytes(
                kCFAllocatorDefault,
                self.width as usize,
                self.height as usize,
                K_CV_PIXEL_FORMAT_TYPE_32BGRA,
                rgba_data.as_mut_ptr() as *mut c_void,
                bytes_per_row,
                Some(pixel_buffer_release_callback),
                Box::into_raw(Box::new(rgba_data)) as *mut c_void,
                ptr::null(),
                &mut pixel_buffer,
            );

            if status != 0 {
                return Err(format!("CVPixelBufferCreateWithBytes failed: {status}"));
            }

            let mut info_flags: VTEncodeInfoFlags = 0;
            let status = VTCompressionSessionEncodeFrame(
                self.session,
                pixel_buffer,
                pts,
                duration,
                ptr::null(),
                ptr::null_mut(),
                &mut info_flags,
            );

            CVPixelBufferRelease(pixel_buffer);

            if status != 0 {
                return Err(format!("VTCompressionSessionEncodeFrame failed: {status}"));
            }
        }

        self.frame_count += 1;
        Ok(())
    }

    /// Pop an encoded packet if available.
    pub fn pop_packet(&self) -> Option<EncodedPacket> {
        self.packet_rx.try_recv().ok()
    }

    /// Convert to a `hang::Frame` for MOQ publishing.
    pub fn pop_hang_frame(&self) -> Option<hang::Frame> {
        let pkt = self.pop_packet()?;
        let frame = hang::Frame {
            payload: pkt.data.into(),
            timestamp: pkt.timestamp,
            keyframe: pkt.keyframe,
        };
        Some(frame)
    }

    pub fn flush(&self) {
        unsafe {
            VTCompressionSessionCompleteFrames(
                self.session,
                CMTimeStruct::invalid(),
            );
        }
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        unsafe {
            VTCompressionSessionCompleteFrames(self.session, CMTimeStruct::invalid());
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as *const c_void);
        }
    }
}

// --- Helpers ---

unsafe fn create_pixel_buffer_attrs(width: u32, height: u32) -> CFDictionaryRef {
    let dict = CFDictionaryCreateMutable(
        kCFAllocatorDefault,
        3,
        &kCFTypeDictionaryKeyCallBacks as *const c_void,
        &kCFTypeDictionaryValueCallBacks as *const c_void,
    );

    let fmt = K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32;
    let fmt_num = CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT32_TYPE, &fmt as *const i32 as *const c_void);
    CFDictionarySetValue(dict, kCVPixelBufferPixelFormatTypeKey as *const c_void, fmt_num as *const c_void);
    CFRelease(fmt_num as *const c_void);

    let w = width as i32;
    let w_num = CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT32_TYPE, &w as *const i32 as *const c_void);
    CFDictionarySetValue(dict, kCVPixelBufferWidthKey as *const c_void, w_num as *const c_void);
    CFRelease(w_num as *const c_void);

    let h = height as i32;
    let h_num = CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT32_TYPE, &h as *const i32 as *const c_void);
    CFDictionarySetValue(dict, kCVPixelBufferHeightKey as *const c_void, h_num as *const c_void);
    CFRelease(h_num as *const c_void);

    dict as CFDictionaryRef
}

unsafe fn configure_session(session: VTCompressionSessionRef, fps: u32) -> Result<(), String> {
    // Real-time encoding
    let status = VTSessionSetProperty(
        session,
        kVTCompressionPropertyKey_RealTime,
        kCFBooleanTrue as CFTypeRef,
    );
    if status != 0 {
        tracing::warn!("Failed to set RealTime: {status}");
    }

    // H.264 Baseline profile (simplest, low latency)
    let status = VTSessionSetProperty(
        session,
        kVTCompressionPropertyKey_ProfileLevel,
        kVTProfileLevel_H264_Baseline_AutoLevel as CFTypeRef,
    );
    if status != 0 {
        tracing::warn!("Failed to set ProfileLevel: {status}");
    }

    // Disable B-frames for low latency
    let status = VTSessionSetProperty(
        session,
        kVTCompressionPropertyKey_AllowFrameReordering,
        kCFBooleanFalse as CFTypeRef,
    );
    if status != 0 {
        tracing::warn!("Failed to set AllowFrameReordering: {status}");
    }

    // Bitrate: ~1.5 Mbps
    let bitrate: i32 = 1_500_000;
    let bitrate_num = CFNumberCreate(
        kCFAllocatorDefault,
        K_CF_NUMBER_SINT32_TYPE,
        &bitrate as *const i32 as *const c_void,
    );
    let status = VTSessionSetProperty(
        session,
        kVTCompressionPropertyKey_AverageBitRate,
        bitrate_num as CFTypeRef,
    );
    CFRelease(bitrate_num as *const c_void);
    if status != 0 {
        tracing::warn!("Failed to set AverageBitRate: {status}");
    }

    // Keyframe interval = fps (1 keyframe per second)
    let kf_interval = fps as i32;
    let kf_num = CFNumberCreate(
        kCFAllocatorDefault,
        K_CF_NUMBER_SINT32_TYPE,
        &kf_interval as *const i32 as *const c_void,
    );
    let status = VTSessionSetProperty(
        session,
        kVTCompressionPropertyKey_MaxKeyFrameInterval,
        kf_num as CFTypeRef,
    );
    CFRelease(kf_num as *const c_void);
    if status != 0 {
        tracing::warn!("Failed to set MaxKeyFrameInterval: {status}");
    }

    Ok(())
}

/// Callback invoked by VideoToolbox when a frame is compressed.
unsafe extern "C" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if status != 0 {
        tracing::warn!("Compression callback error: {status}");
        return;
    }
    if sample_buffer.is_null() || CMSampleBufferIsValid(sample_buffer) == 0 {
        return;
    }

    let ctx = &*(output_callback_ref_con as *const CallbackContext);

    // Get presentation timestamp
    let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);
    let timestamp_micros = if pts.timescale > 0 {
        ((pts.value as f64 / pts.timescale as f64) * 1_000_000.0) as u64
    } else {
        0
    };

    let timestamp = match Timestamp::from_micros(timestamp_micros) {
        Ok(ts) => ts,
        Err(_) => return,
    };

    // Check if keyframe
    // We detect keyframes by checking if we can get SPS/PPS from the format description
    let format_desc = CMSampleBufferGetFormatDescription(sample_buffer);
    let keyframe = if !format_desc.is_null() {
        let mut param_ptr: *const u8 = ptr::null();
        let mut param_size: usize = 0;
        let mut param_count: usize = 0;
        let mut nal_header_len: i32 = 0;
        let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_desc,
            0,
            &mut param_ptr,
            &mut param_size,
            &mut param_count,
            &mut nal_header_len,
        );
        status == 0 && param_count > 0
    } else {
        false
    };

    // Get encoded data
    let block_buffer = CMSampleBufferGetDataBuffer(sample_buffer);
    if block_buffer.is_null() {
        return;
    }

    let data_len = CMBlockBufferGetDataLength(block_buffer);
    if data_len == 0 {
        return;
    }

    // Build output: for keyframes, prepend SPS/PPS as Annex B NAL units
    let mut output = Vec::new();

    if keyframe && !format_desc.is_null() {
        // Extract SPS and PPS
        for i in 0..2 {
            let mut param_ptr: *const u8 = ptr::null();
            let mut param_size: usize = 0;
            let mut param_count: usize = 0;
            let mut nal_header_len: i32 = 0;
            let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format_desc,
                i,
                &mut param_ptr,
                &mut param_size,
                &mut param_count,
                &mut nal_header_len,
            );
            if status == 0 && !param_ptr.is_null() && param_size > 0 {
                // Annex B start code
                output.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                output.extend_from_slice(std::slice::from_raw_parts(param_ptr, param_size));
            }
        }
    }

    // Read the encoded NAL units from block buffer
    let mut raw_data = vec![0u8; data_len];
    let status = CMBlockBufferCopyDataBytes(
        block_buffer,
        0,
        data_len,
        raw_data.as_mut_ptr(),
    );
    if status != 0 {
        tracing::warn!("CMBlockBufferCopyDataBytes failed: {status}");
        return;
    }

    // Convert AVCC (length-prefixed) to Annex B (start code prefixed)
    let mut offset = 0;
    while offset + 4 <= raw_data.len() {
        let nal_len = u32::from_be_bytes([
            raw_data[offset],
            raw_data[offset + 1],
            raw_data[offset + 2],
            raw_data[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + nal_len > raw_data.len() {
            break;
        }

        output.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        output.extend_from_slice(&raw_data[offset..offset + nal_len]);
        offset += nal_len;
    }

    let packet = EncodedPacket {
        data: Bytes::from(output),
        timestamp,
        keyframe,
    };

    let _ = ctx.packet_tx.send(packet);
}

/// Release callback for CVPixelBufferCreateWithBytes.
/// Frees the Vec<u8> that backs the pixel buffer.
unsafe extern "C" fn pixel_buffer_release_callback(
    release_ref_con: *mut c_void,
    _base_address: *const c_void,
) {
    // Reconstruct and drop the Box<Vec<u8>>
    let _ = Box::from_raw(release_ref_con as *mut Vec<u8>);
}
