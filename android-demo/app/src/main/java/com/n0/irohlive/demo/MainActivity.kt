package com.n0.irohlive.demo

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Rect
import android.os.Bundle
import android.util.Log
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Main activity for the iroh-live Android demo.
 *
 * Phase 1: subscribe-only viewer. Connects via a ticket string, decodes
 * H.264 video in Rust, and renders RGBA frames onto a SurfaceView.
 */
class MainActivity : AppCompatActivity() {
    companion object {
        private const val TAG = "IrohLiveDemo"

        // Default frame buffer dimensions. Updated when the first frame arrives.
        private const val DEFAULT_WIDTH = 1280
        private const val DEFAULT_HEIGHT = 720
    }

    private lateinit var ticketInput: EditText
    private lateinit var connectButton: Button
    private lateinit var disconnectButton: Button
    private lateinit var videoSurface: SurfaceView
    private lateinit var statusText: TextView

    private var sessionHandle: Long = 0
    private var renderJob: Job? = null
    private var surfaceReady = false
    private var frameBuffer: ByteBuffer? = null
    private var frameBitmap: Bitmap? = null

    // Permission launcher for camera and microphone.
    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { grants ->
        val allGranted = grants.values.all { it }
        if (!allGranted) {
            Log.w(TAG, "Not all permissions were granted: $grants")
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        ticketInput = findViewById(R.id.ticketInput)
        connectButton = findViewById(R.id.connectButton)
        disconnectButton = findViewById(R.id.disconnectButton)
        videoSurface = findViewById(R.id.videoSurface)
        statusText = findViewById(R.id.statusText)

        // Request permissions up front so they are ready for Phase 2 camera capture.
        permissionLauncher.launch(
            arrayOf(
                android.Manifest.permission.CAMERA,
                android.Manifest.permission.RECORD_AUDIO,
            )
        )

        videoSurface.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                surfaceReady = true
            }

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                surfaceReady = false
            }
        })

        connectButton.setOnClickListener { onConnect() }
        disconnectButton.setOnClickListener { onDisconnect() }
    }

    override fun onDestroy() {
        super.onDestroy()
        if (sessionHandle != 0L) {
            IrohBridge.disconnect(sessionHandle)
            sessionHandle = 0
        }
    }

    private fun onConnect() {
        val ticket = ticketInput.text.toString().trim()
        if (ticket.isEmpty()) {
            statusText.text = "Enter a ticket string"
            return
        }

        connectButton.isEnabled = false
        statusText.text = "Connecting..."

        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.connect(ticket)
            }

            if (handle == 0L) {
                statusText.text = "Connection failed"
                connectButton.isEnabled = true
                return@launch
            }

            sessionHandle = handle
            statusText.text = "Connected"
            disconnectButton.isEnabled = true

            startRenderLoop()
        }
    }

    private fun onDisconnect() {
        renderJob?.cancel()
        renderJob = null

        val handle = sessionHandle
        sessionHandle = 0

        if (handle != 0L) {
            lifecycleScope.launch(Dispatchers.IO) {
                IrohBridge.disconnect(handle)
            }
        }

        disconnectButton.isEnabled = false
        connectButton.isEnabled = true
        statusText.text = "Disconnected"
    }

    /**
     * Runs a coroutine that polls for decoded frames and blits them to the SurfaceView.
     *
     * The render loop runs on [Dispatchers.Default] and targets ~30 fps. Each
     * frame is copied from the Rust side into a direct ByteBuffer, wrapped in a
     * Bitmap, and drawn onto the Surface canvas.
     */
    private fun startRenderLoop() {
        // Allocate a direct ByteBuffer for frame data (RGBA, 4 bytes per pixel).
        val width = DEFAULT_WIDTH
        val height = DEFAULT_HEIGHT
        val bufferSize = width * height * 4

        frameBuffer = ByteBuffer.allocateDirect(bufferSize).order(ByteOrder.nativeOrder())
        frameBitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)

        renderJob = lifecycleScope.launch(Dispatchers.Default) {
            val buf = frameBuffer!!
            val bmp = frameBitmap!!

            while (isActive && sessionHandle != 0L) {
                buf.clear()
                val hasFrame = IrohBridge.nextFrame(sessionHandle, buf)

                if (hasFrame && surfaceReady) {
                    buf.rewind()
                    bmp.copyPixelsFromBuffer(buf)

                    val holder = videoSurface.holder
                    val canvas: Canvas? = holder.lockCanvas()
                    if (canvas != null) {
                        try {
                            val dst = Rect(0, 0, canvas.width, canvas.height)
                            canvas.drawBitmap(bmp, null, dst, null)
                        } finally {
                            holder.unlockCanvasAndPost(canvas)
                        }
                    }
                }

                // Target ~30 fps; yield if no frame was available.
                delay(if (hasFrame) 16L else 50L)
            }
        }
    }
}
