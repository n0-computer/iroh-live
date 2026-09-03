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
     * Publishes this node's side of a call and waits for a peer to dial it.
     *
     * Returns immediately with an opaque session handle (non-zero on success,
     * 0 on failure), so [getTicket] can show a code before any peer exists.
     * The peer's tracks arrive later; [callConnected] says when.
     */
    external fun answer(cameraWidth: Int, cameraHeight: Int): Long

    /**
     * Whether a call started by [answer] has a peer on it yet.
     */
    external fun callConnected(handle: Long): Boolean

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
     * Creates the EGL context + GL renderer for the given Android Surface.
     *
     * Must be called from the render thread. Replaces the Kotlin-side EGL setup.
     */
    external fun initSurface(handle: Long, surface: android.view.Surface)

    /**
     * Tears down the EGL surface and context. Called when the render loop exits.
     */
    external fun teardownSurface(handle: Long)

    /**
     * Polls for the next decoded frame, renders it, and swaps EGL buffers.
     *
     * [rotationDegrees] is the camera sensor rotation (0/90/180/270).
     * Returns true if a frame was rendered.
     */
    external fun renderNextFrame(
        handle: Long, surfaceWidth: Int, surfaceHeight: Int, rotationDegrees: Int
    ): Boolean

    /**
     * Publishes camera + mic as a broadcast. Returns a session handle.
     */
    external fun publish(name: String, cameraWidth: Int, cameraHeight: Int): Long

    /**
     * Returns the connection ticket string for a published broadcast.
     */
    external fun getTicket(handle: Long): String

    /**
     * Returns available video rendition names as a newline-separated string.
     */
    external fun getRenditions(handle: Long): String

    /**
     * Switches to a different video rendition by name.
     */
    external fun switchRendition(handle: Long, renditionName: String)

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
