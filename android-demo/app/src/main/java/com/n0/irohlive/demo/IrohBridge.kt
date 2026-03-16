package com.n0.irohlive.demo

import java.nio.ByteBuffer

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
     * Polls for the next decoded video frame.
     *
     * If a new RGBA frame is available, copies it into [buffer] and returns true.
     * The buffer must be a direct ByteBuffer large enough for width * height * 4 bytes.
     */
    external fun nextFrame(handle: Long, buffer: ByteBuffer): Boolean

    /**
     * Starts publishing a broadcast with the given name.
     *
     * After this call, camera frames can be pushed via [pushCameraFrame].
     */
    external fun startPublish(handle: Long, name: String)

    /**
     * Pushes a camera frame (RGBA byte array) into the publish pipeline.
     *
     * [data] must contain width * height * 4 bytes of RGBA pixel data.
     */
    external fun pushCameraFrame(handle: Long, data: ByteArray, width: Int, height: Int)

    /**
     * Disconnects and frees the session handle.
     *
     * The handle must not be used after this call.
     */
    external fun disconnect(handle: Long)
}
