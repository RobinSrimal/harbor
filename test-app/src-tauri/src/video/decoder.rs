//! H.264 hardware decoder using Apple VideoToolbox via raw FFI.
//!
//! Uses VTDecompressionSession to decode Annex B H.264 NAL units
//! back to BGRA pixel buffers with hardware acceleration.

use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc;

// Reuse type aliases from encoder
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFTypeRef = *const c_void;
type CFIndex = isize;

type CMSampleBufferRef = *mut c_void;
type CMFormatDescriptionRef = *mut c_void;
type CMBlockBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type CVReturn = i32;
type OSStatus = i32;

type VTDecompressionSessionRef = *mut c_void;
type VTDecodeInfoFlags = u32;

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
}

const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: CFAllocatorRef;
    static kCFAllocatorNull: CFAllocatorRef;
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
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;

    fn CMSampleBufferCreate(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        data_ready: u8, // Boolean
        make_data_ready_callback: *const c_void,
        make_data_ready_refcon: *const c_void,
        format_description: CMFormatDescriptionRef,
        num_samples: isize,              // CMItemCount = CFIndex = long
        num_sample_timing_entries: isize, // CMItemCount
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: isize,   // CMItemCount
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CMSampleTimingInfo {
    duration: CMTimeStruct,
    presentation_time_stamp: CMTimeStruct,
    decode_time_stamp: CMTimeStruct,
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;

    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> CVReturn;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> CVReturn;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
}

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    decompression_output_callback: Option<
        unsafe extern "C" fn(
            *mut c_void,        // decompressionOutputRefCon
            *mut c_void,        // sourceFrameRefCon
            OSStatus,           // status
            VTDecodeInfoFlags,  // infoFlags
            CVPixelBufferRef,   // imageBuffer
            CMTimeStruct,       // presentationTimeStamp
            CMTimeStruct,       // presentationDuration
        ),
    >,
    decompression_output_ref_con: *mut c_void,
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMFormatDescriptionRef,
        video_decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        decompression_session_out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: u32,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut VTDecodeInfoFlags,
    ) -> OSStatus;

    fn VTDecompressionSessionWaitForAsynchronousFrames(
        session: VTDecompressionSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);
}

/// A decoded video frame (RGBA pixels).
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba_data: Vec<u8>,
    pub timestamp_micros: u64,
}

struct DecoderCallbackContext {
    frame_tx: mpsc::Sender<DecodedFrame>,
}

pub struct H264Decoder {
    session: VTDecompressionSessionRef,
    format_desc: CMFormatDescriptionRef,
    width: u32,
    height: u32,
    frame_rx: mpsc::Receiver<DecodedFrame>,
    _callback_ctx: Box<DecoderCallbackContext>,
    // Stored SPS/PPS to detect changes
    current_sps: Vec<u8>,
    current_pps: Vec<u8>,
    frame_count: u64,
}

unsafe impl Send for H264Decoder {}

impl H264Decoder {
    pub fn new(width: u32, height: u32) -> Self {
        let (frame_tx, frame_rx) = mpsc::channel();
        let callback_ctx = Box::new(DecoderCallbackContext { frame_tx });

        Self {
            session: ptr::null_mut(),
            format_desc: ptr::null_mut(),
            width,
            height,
            frame_rx,
            _callback_ctx: callback_ctx,
            current_sps: Vec::new(),
            current_pps: Vec::new(),
            frame_count: 0,
        }
    }

    /// Push an Annex B H.264 packet. Parses NAL units, extracts SPS/PPS,
    /// creates/recreates the decoder session as needed, and decodes slice NALs.
    pub fn push_packet(&mut self, data: &[u8]) -> Result<(), String> {
        let nals = parse_annex_b_nals(data);
        if nals.is_empty() {
            return Ok(());
        }

        let mut sps: Option<&[u8]> = None;
        let mut pps: Option<&[u8]> = None;
        let mut slices: Vec<&[u8]> = Vec::new();

        for nal in &nals {
            if nal.is_empty() {
                continue;
            }
            let nal_type = nal[0] & 0x1F;
            match nal_type {
                7 => sps = Some(*nal), // SPS
                8 => pps = Some(*nal), // PPS
                1 | 5 => slices.push(*nal), // Non-IDR slice / IDR slice
                _ => {} // SEI, AUD, etc. - ignore
            }
        }

        // If we got new SPS/PPS, rebuild the decoder session
        if let (Some(sps_data), Some(pps_data)) = (sps, pps) {
            if sps_data != self.current_sps.as_slice() || pps_data != self.current_pps.as_slice() {
                self.current_sps = sps_data.to_vec();
                self.current_pps = pps_data.to_vec();
                self.create_session(sps_data, pps_data)?;
            }
        }

        // Decode slice NALs
        if self.session.is_null() {
            // No session yet - waiting for SPS/PPS
            return Ok(());
        }

        for slice_nal in &slices {
            if let Err(e) = self.decode_nal(slice_nal) {
                eprintln!("[VIDEO-RX] Decode NAL error (type {}): {e}", slice_nal[0] & 0x1F);
            }
        }

        Ok(())
    }

    fn create_session(&mut self, sps: &[u8], pps: &[u8]) -> Result<(), String> {
        unsafe {
            // Invalidate old session
            if !self.session.is_null() {
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session as *const c_void);
                self.session = ptr::null_mut();
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc as *const c_void);
                self.format_desc = ptr::null_mut();
            }

            // Create format description from SPS/PPS
            let param_sets: [*const u8; 2] = [sps.as_ptr(), pps.as_ptr()];
            let param_sizes: [usize; 2] = [sps.len(), pps.len()];

            let status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                kCFAllocatorDefault,
                2,
                param_sets.as_ptr(),
                param_sizes.as_ptr(),
                4, // NAL unit header length (AVCC style)
                &mut self.format_desc,
            );
            if status != 0 {
                return Err(format!("CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {status}"));
            }

            // Create destination pixel buffer attributes (BGRA output only, no size constraint)
            // Let VideoToolbox use the actual stream dimensions from SPS.
            let dest_attrs = create_pixel_format_attrs();

            // Create output callback record
            let ctx_ptr = &*self._callback_ctx as *const DecoderCallbackContext as *mut c_void;
            let callback_record = VTDecompressionOutputCallbackRecord {
                decompression_output_callback: Some(decompression_output_callback),
                decompression_output_ref_con: ctx_ptr,
            };

            let status = VTDecompressionSessionCreate(
                kCFAllocatorDefault,
                self.format_desc,
                ptr::null(),   // decoder specification
                dest_attrs,    // destination image buffer attributes
                &callback_record as *const VTDecompressionOutputCallbackRecord,
                &mut self.session,
            );

            if !dest_attrs.is_null() {
                CFRelease(dest_attrs);
            }

            if status != 0 {
                return Err(format!("VTDecompressionSessionCreate failed: {status}"));
            }

            eprintln!("[VIDEO-RX] H264Decoder session created: {}x{}", self.width, self.height);
        }
        Ok(())
    }

    fn decode_nal(&mut self, nal: &[u8]) -> Result<(), String> {
        unsafe {
            // Convert Annex B NAL to AVCC (length-prefixed) for VideoToolbox
            let nal_len = nal.len() as u32;
            let mut avcc_data = Vec::with_capacity(4 + nal.len());
            avcc_data.extend_from_slice(&nal_len.to_be_bytes());
            avcc_data.extend_from_slice(nal);

            let data_len = avcc_data.len();
            let data_ptr = avcc_data.as_mut_ptr();

            // Create block buffer referencing our Vec's memory.
            // We pass ptr::null() as block_allocator so CM won't try to free it —
            // the Vec stays alive until end of this function, and we call
            // WaitForAsynchronousFrames before returning to ensure decode completes.
            let mut block_buffer: CMBlockBufferRef = ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                kCFAllocatorDefault,
                data_ptr as *mut c_void,
                data_len,
                kCFAllocatorNull, // don't free — Vec owns the memory
                ptr::null(),
                0,
                data_len,
                0,
                &mut block_buffer,
            );
            if status != 0 {
                return Err(format!("CMBlockBufferCreateWithMemoryBlock failed: {status}"));
            }

            // Create CMSampleBuffer
            self.frame_count += 1;
            let timing = CMSampleTimingInfo {
                duration: CMTimeStruct::make(1, 30),
                presentation_time_stamp: CMTimeStruct::make(self.frame_count as i64, 30),
                decode_time_stamp: CMTimeStruct::make(self.frame_count as i64, 30),
            };
            let sample_size = data_len;

            let mut sample_buffer: CMSampleBufferRef = ptr::null_mut();
            let status = CMSampleBufferCreate(
                kCFAllocatorDefault,
                block_buffer,
                1, // dataReady = true
                ptr::null(),
                ptr::null(),
                self.format_desc,
                1, // numSamples
                1, // numSampleTimingEntries
                &timing as *const CMSampleTimingInfo,
                1, // numSampleSizeEntries
                &sample_size as *const usize,
                &mut sample_buffer,
            );

            if status != 0 {
                CFRelease(block_buffer as *const c_void);
                return Err(format!("CMSampleBufferCreate failed: {status}"));
            }

            // Decode — use async flag and wait, so VT can retain the
            // sample/block buffers as long as it needs.
            let mut info_flags: VTDecodeInfoFlags = 0;
            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buffer,
                1, // kVTDecodeFrame_EnableAsynchronousDecompression
                ptr::null_mut(),
                &mut info_flags,
            );

            if status != 0 {
                CFRelease(sample_buffer as *const c_void);
                CFRelease(block_buffer as *const c_void);
                return Err(format!("VTDecompressionSessionDecodeFrame failed: {status}"));
            }

            // Wait for async decode to complete before releasing CM objects
            // and before avcc_data (which owns the underlying memory) is dropped.
            VTDecompressionSessionWaitForAsynchronousFrames(self.session);

            CFRelease(sample_buffer as *const c_void);
            CFRelease(block_buffer as *const c_void);
            // avcc_data drops here, freeing the memory after CM is done with it
            drop(avcc_data);
        }
        Ok(())
    }

    /// Pop a decoded frame if available.
    pub fn pop_frame(&self) -> Option<DecodedFrame> {
        self.frame_rx.try_recv().ok()
    }

    pub fn flush(&self) {
        if !self.session.is_null() {
            unsafe {
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
            }
        }
    }
}

impl Drop for H264Decoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session as *const c_void);
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc as *const c_void);
            }
        }
    }
}

// --- Helpers ---

/// Parse Annex B NAL units (split on 0x00000001 or 0x000001 start codes).
fn parse_annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let len = data.len();

    // Find first start code
    while i < len {
        if i + 4 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            i += 4;
            break;
        } else if i + 3 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            i += 3;
            break;
        }
        i += 1;
    }

    let mut nal_start = i;

    while i < len {
        // Look for next start code
        if i + 4 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            if i > nal_start {
                nals.push(&data[nal_start..i]);
            }
            i += 4;
            nal_start = i;
        } else if i + 3 <= len && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if i > nal_start {
                nals.push(&data[nal_start..i]);
            }
            i += 3;
            nal_start = i;
        } else {
            i += 1;
        }
    }

    // Last NAL
    if nal_start < len {
        nals.push(&data[nal_start..len]);
    }

    nals
}

/// Create pixel buffer attributes that only specify BGRA format.
/// Dimensions are derived from the H.264 stream's SPS.
unsafe fn create_pixel_format_attrs() -> CFDictionaryRef {
    let dict = CFDictionaryCreateMutable(
        kCFAllocatorDefault,
        1,
        &kCFTypeDictionaryKeyCallBacks as *const c_void,
        &kCFTypeDictionaryValueCallBacks as *const c_void,
    );

    let fmt = K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32;
    let fmt_num = CFNumberCreate(kCFAllocatorDefault, K_CF_NUMBER_SINT32_TYPE, &fmt as *const i32 as *const c_void);
    CFDictionarySetValue(dict, kCVPixelBufferPixelFormatTypeKey as *const c_void, fmt_num as *const c_void);
    CFRelease(fmt_num as *const c_void);

    dict as CFDictionaryRef
}

/// Callback invoked by VideoToolbox when a frame is decompressed.
unsafe extern "C" fn decompression_output_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: CVPixelBufferRef,
    presentation_time_stamp: CMTimeStruct,
    _presentation_duration: CMTimeStruct,
) {
    if status != 0 {
        eprintln!("[VIDEO-RX] Decompression callback error: {status}");
        return;
    }
    if image_buffer.is_null() {
        eprintln!("[VIDEO-RX] Decompression callback: null image buffer");
        return;
    }

    let ctx = &*(decompression_output_ref_con as *const DecoderCallbackContext);

    // Lock pixel buffer and read BGRA data
    let lock_status = CVPixelBufferLockBaseAddress(image_buffer, 1); // kCVPixelBufferLock_ReadOnly = 1
    if lock_status != 0 {
        tracing::warn!("CVPixelBufferLockBaseAddress failed: {lock_status}");
        return;
    }

    let base_address = CVPixelBufferGetBaseAddress(image_buffer);
    let width = CVPixelBufferGetWidth(image_buffer) as u32;
    let height = CVPixelBufferGetHeight(image_buffer) as u32;
    let bytes_per_row = CVPixelBufferGetBytesPerRow(image_buffer);

    if !base_address.is_null() && width > 0 && height > 0 {
        // Copy BGRA data and convert to RGBA
        let mut rgba_data = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let row_ptr = (base_address as *const u8).add(row as usize * bytes_per_row);
            for col in 0..width {
                let pixel = row_ptr.add(col as usize * 4);
                // BGRA → RGBA
                rgba_data.push(*pixel.add(2)); // R (was B)
                rgba_data.push(*pixel.add(1)); // G
                rgba_data.push(*pixel.add(0)); // B (was R)
                rgba_data.push(*pixel.add(3)); // A
            }
        }

        let timestamp_micros = if presentation_time_stamp.timescale > 0 {
            ((presentation_time_stamp.value as f64 / presentation_time_stamp.timescale as f64)
                * 1_000_000.0) as u64
        } else {
            0
        };

        let frame = DecodedFrame {
            width,
            height,
            rgba_data,
            timestamp_micros,
        };

        let _ = ctx.frame_tx.send(frame);
    }

    CVPixelBufferUnlockBaseAddress(image_buffer, 1);
}
