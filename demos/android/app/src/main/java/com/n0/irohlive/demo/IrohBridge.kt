package com.n0.irohlive.demo

/**
 * JNI bridge to the Rust iroh-live library.
 *
 * All methods are blocking and must be called from a background thread.
 * The native library creates a tokio runtime internally.
 */
object IrohBridge {
    init {
        System.loadLibrary("iroh_live_android")
    }

    /**
     * Connects to a remote broadcast using a ticket string.
     *
     * Returns an opaque session handle (non-zero on success, 0 on failure).
     */
    external fun connect(ticket: String): Long

    /**
     * Returns video dimensions packed as `(width shl 32) or height`, or 0 if unknown.
     */
    external fun getVideoDimensions(handle: Long): Long

    /**
     * Returns a raw AHardwareBuffer pointer for the latest decoded frame.
     *
     * The buffer has an acquired reference and must be released via
     * [releaseHardwareBuffer] when the caller is done (after GL import
     * and rendering).
     *
     * Returns 0 if no GPU frame is available.
     */
    external fun nextHardwareBuffer(handle: Long): Long

    /**
     * Releases an AHardwareBuffer previously returned by [nextHardwareBuffer].
     *
     * Must be called exactly once per non-zero return from [nextHardwareBuffer].
     */
    external fun releaseHardwareBuffer(bufferPtr: Long)

    /**
     * Dials a remote peer using a call ticket string.
     *
     * Sets up camera publishing (720p H.264 HW encoding) and microphone
     * audio, then subscribes to the remote peer's media.
     *
     * Returns an opaque session handle (non-zero on success, 0 on failure).
     *
     * [cameraWidth] and [cameraHeight] configure the encoder for the
     * camera resolution that will be pushed via [pushCameraFrame].
     */
    external fun dial(ticket: String, cameraWidth: Int, cameraHeight: Int): Long

    /**
     * Pushes a camera frame (RGBA byte array) into the publish pipeline.
     *
     * [data] must contain width * height * 4 bytes of RGBA pixel data.
     */
    external fun pushCameraFrame(handle: Long, data: ByteArray, width: Int, height: Int)

    /**
     * Pushes a camera frame as NV12 planes into the publish pipeline.
     *
     * [yData] is the luminance plane, [uvData] is the interleaved chroma plane.
     * Strides may differ from width due to hardware padding.
     */
    external fun pushCameraNv12(
        handle: Long,
        yData: ByteArray, uvData: ByteArray,
        width: Int, height: Int,
        yStride: Int, uvStride: Int
    )

    /**
     * Starts a direct camera passthrough pipeline (no encode/decode).
     *
     * Returns an opaque session handle (non-zero on success, 0 on failure).
     */
    external fun startDirect(cameraWidth: Int, cameraHeight: Int): Long

    /**
     * Starts a local H264 encode→decode pipeline (no network).
     *
     * Returns an opaque session handle (non-zero on success, 0 on failure).
     */
    external fun startH264(cameraWidth: Int, cameraHeight: Int): Long

    /**
     * Initializes the Rust-side GLES2 renderer on the current GL thread.
     *
     * Must be called after eglMakeCurrent. [eglDisplayPtr] is the native
     * EGLDisplay pointer.
     */
    external fun initRenderer(handle: Long, eglDisplayPtr: Long)

    /**
     * Polls for the next decoded frame and renders it via the Rust renderer.
     *
     * [rotationDegrees] is the camera sensor rotation (0/90/180/270) to
     * correct for sensor orientation.
     *
     * Returns true if a frame was rendered (caller should swap buffers).
     */
    external fun renderNextFrame(
        handle: Long, surfaceWidth: Int, surfaceHeight: Int, rotationDegrees: Int
    ): Boolean

    /**
     * Returns a human-readable status string with encode/decode stats.
     *
     * Returns an empty string if the handle is invalid or no stats are
     * available yet.
     */
    external fun getStatusLine(handle: Long): String

    /**
     * Disconnects and frees the session handle.
     *
     * The handle must not be used after this call.
     */
    external fun disconnect(handle: Long)
}
