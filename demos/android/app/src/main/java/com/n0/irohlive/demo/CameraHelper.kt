package com.n0.irohlive.demo

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Manages Camera2 capture and forwards RGBA frames to the Rust publish pipeline.
 *
 * Phase 2 implementation: opens the first back-facing camera, captures YUV_420_888
 * frames, converts to RGBA, and pushes them into the JNI bridge.
 */
class CameraHelper(private val context: Context) {
    companion object {
        private const val TAG = "CameraHelper"
        private const val CAPTURE_WIDTH = 640
        private const val CAPTURE_HEIGHT = 480
    }

    private var cameraDevice: CameraDevice? = null
    private var captureSession: CameraCaptureSession? = null
    private var imageReader: ImageReader? = null
    private var backgroundThread: HandlerThread? = null
    private var backgroundHandler: Handler? = null
    private var sessionHandle: Long = 0

    /** Starts camera capture and pushes frames to the given session handle. */
    fun start(handle: Long) {
        sessionHandle = handle
        startBackgroundThread()

        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED
        ) {
            Log.e(TAG, "Camera permission not granted")
            return
        }

        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager

        // Find the first back-facing camera.
        val cameraId = manager.cameraIdList.firstOrNull { id ->
            val chars = manager.getCameraCharacteristics(id)
            chars.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_BACK
        } ?: run {
            Log.e(TAG, "No back-facing camera found")
            return
        }

        imageReader = ImageReader.newInstance(
            CAPTURE_WIDTH, CAPTURE_HEIGHT, ImageFormat.YUV_420_888, 2
        ).apply {
            setOnImageAvailableListener({ reader ->
                val image = reader.acquireLatestImage() ?: return@setOnImageAvailableListener
                try {
                    // Convert YUV to RGBA and push to Rust.
                    val rgba = yuvToRgba(image)
                    IrohBridge.pushCameraFrame(sessionHandle, rgba, image.width, image.height)
                } finally {
                    image.close()
                }
            }, backgroundHandler)
        }

        try {
            manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    cameraDevice = camera
                    createCaptureSession(camera)
                }

                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    cameraDevice = null
                }

                override fun onError(camera: CameraDevice, error: Int) {
                    Log.e(TAG, "Camera error: $error")
                    camera.close()
                    cameraDevice = null
                }
            }, backgroundHandler)
        } catch (e: SecurityException) {
            Log.e(TAG, "Camera permission denied", e)
        }
    }

    /** Stops camera capture and releases resources. */
    fun stop() {
        captureSession?.close()
        captureSession = null
        cameraDevice?.close()
        cameraDevice = null
        imageReader?.close()
        imageReader = null
        stopBackgroundThread()
    }

    private fun createCaptureSession(camera: CameraDevice) {
        val reader = imageReader ?: return
        val surface = reader.surface

        camera.createCaptureSession(
            listOf(surface),
            object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(session: CameraCaptureSession) {
                    captureSession = session
                    val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW).apply {
                        addTarget(surface)
                        set(CaptureRequest.CONTROL_AF_MODE, CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
                    }
                    session.setRepeatingRequest(request.build(), null, backgroundHandler)
                }

                override fun onConfigureFailed(session: CameraCaptureSession) {
                    Log.e(TAG, "Capture session configuration failed")
                }
            },
            backgroundHandler
        )
    }

    private fun startBackgroundThread() {
        backgroundThread = HandlerThread("CameraBackground").also { it.start() }
        backgroundHandler = Handler(backgroundThread!!.looper)
    }

    private fun stopBackgroundThread() {
        backgroundThread?.quitSafely()
        try {
            backgroundThread?.join()
        } catch (_: InterruptedException) {}
        backgroundThread = null
        backgroundHandler = null
    }

    /**
     * Converts a YUV_420_888 Image to an RGBA byte array.
     *
     * **Warning: This is a pixel-by-pixel CPU loop and will cause ANR at
     * production resolutions (720p+).** It exists only for this demo at
     * 640x480. The main `MainActivity` path uses direct NV12 plane push
     * instead, which avoids this conversion entirely.
     *
     * TODO(perf): Replace with libyuv JNI bindings, Android `ImageFormat`
     * native conversion, or a GPU compute shader if this code path is ever
     * used at higher resolutions.
     */
    private fun yuvToRgba(image: android.media.Image): ByteArray {
        val w = image.width
        val h = image.height
        val yPlane = image.planes[0]
        val uPlane = image.planes[1]
        val vPlane = image.planes[2]

        val yBuffer = yPlane.buffer
        val uBuffer = uPlane.buffer
        val vBuffer = vPlane.buffer

        val yRowStride = yPlane.rowStride
        val uvRowStride = uPlane.rowStride
        val uvPixelStride = uPlane.pixelStride

        val rgba = ByteArray(w * h * 4)

        for (row in 0 until h) {
            for (col in 0 until w) {
                val y = (yBuffer.get(row * yRowStride + col).toInt() and 0xFF)
                val uvRow = row / 2
                val uvCol = col / 2
                val uvIndex = uvRow * uvRowStride + uvCol * uvPixelStride
                val u = (uBuffer.get(uvIndex).toInt() and 0xFF) - 128
                val v = (vBuffer.get(uvIndex).toInt() and 0xFF) - 128

                var r = y + (1.370705f * v).toInt()
                var g = y - (0.337633f * u).toInt() - (0.698001f * v).toInt()
                var b = y + (1.732446f * u).toInt()

                r = r.coerceIn(0, 255)
                g = g.coerceIn(0, 255)
                b = b.coerceIn(0, 255)

                val offset = (row * w + col) * 4
                rgba[offset] = r.toByte()
                rgba[offset + 1] = g.toByte()
                rgba[offset + 2] = b.toByte()
                rgba[offset + 3] = 0xFF.toByte()
            }
        }

        return rgba
    }
}
