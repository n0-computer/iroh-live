package com.n0.irohlive.demo

import android.graphics.ImageFormat
import android.os.Bundle
import android.util.Log
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import java.nio.ByteBuffer

/**
 * Main activity for the iroh-live Android demo.
 *
 * Supports two modes:
 * - **Watch**: subscribe-only viewer via a broadcast ticket.
 * - **Call**: dials a remote peer, publishes camera at 720p H.264 HW encoding
 *   and microphone audio (Opus), while subscribing to the remote's media.
 *
 * Video frames are decoded in Rust (zero-copy HW decoder) and rendered via
 * OpenGL ES external texture import from AHardwareBuffer.
 */
class MainActivity : AppCompatActivity() {
    companion object {
        private const val TAG = "IrohLiveDemo"
        private const val CAMERA_WIDTH = 1280
        private const val CAMERA_HEIGHT = 720
    }

    private lateinit var ticketInput: EditText
    private lateinit var scanButton: Button
    private lateinit var connectButton: Button
    private lateinit var dialButton: Button
    private lateinit var disconnectButton: Button
    private lateinit var directButton: Button
    private lateinit var h264Button: Button
    private lateinit var publishButton: Button
    private lateinit var copyTicketButton: Button
    private lateinit var renditionSpinner: android.widget.Spinner
    private lateinit var videoSurface: SurfaceView
    private lateinit var cameraPreview: PreviewView
    private lateinit var statusText: TextView

    @Volatile
    private var sessionHandle: Long = 0
    private var renderJob: Job? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private var surfaceReady = false
    /** Camera sensor rotation in degrees (0/90/180/270). */
    private var sensorRotation = 0

    // (EGL/GL state is now fully managed in Rust.)

    // QR code scanner launcher.
    private val scanLauncher = registerForActivityResult(ScanContract()) { result ->
        if (result.contents != null) {
            ticketInput.setText(result.contents)
            onConnect()
        }
    }

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
        scanButton = findViewById(R.id.scanButton)
        connectButton = findViewById(R.id.connectButton)
        dialButton = findViewById(R.id.dialButton)
        disconnectButton = findViewById(R.id.disconnectButton)
        directButton = findViewById(R.id.directButton)
        h264Button = findViewById(R.id.h264Button)
        publishButton = findViewById(R.id.publishButton)
        copyTicketButton = findViewById(R.id.copyTicketButton)
        renditionSpinner = findViewById(R.id.renditionSpinner)
        videoSurface = findViewById(R.id.videoSurface)
        cameraPreview = findViewById(R.id.cameraPreview)
        statusText = findViewById(R.id.statusText)

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

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                surfaceWidth = width
                surfaceHeight = height
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                surfaceReady = false
            }
        })

        scanButton.setOnClickListener { onScan() }
        connectButton.setOnClickListener { onConnect() }
        dialButton.setOnClickListener { onDial() }
        disconnectButton.setOnClickListener { onDisconnect() }
        directButton.setOnClickListener { onDebugDirect() }
        h264Button.setOnClickListener { onDebugH264() }
        publishButton.setOnClickListener { onPublish() }
        copyTicketButton.setOnClickListener { onCopyTicket() }

        // Handle iroh-live: URI intents (from QR scanner apps, links, etc.)
        intent?.data?.let { uri ->
            if (uri.scheme == "iroh-live") {
                ticketInput.setText(uri.toString())
                onConnect()
            }
        }
    }

    override fun onDestroy() {
        // Cancel the render loop before freeing the session handle, so the
        // loop cannot race with disconnect on the native side.
        renderJob?.cancel()
        renderJob = null
        if (sessionHandle != 0L) {
            IrohBridge.disconnect(sessionHandle)
            sessionHandle = 0
        }
        super.onDestroy()
    }

    private fun onScan() {
        val options = ScanOptions().apply {
            setDesiredBarcodeFormats(ScanOptions.QR_CODE)
            setPrompt("Scan iroh-live QR code")
            setBeepEnabled(false)
            setOrientationLocked(false)
        }
        scanLauncher.launch(options)
    }

    private fun onConnect() {
        val ticket = ticketInput.text.toString().trim()
        if (ticket.isEmpty()) {
            statusText.text = "Enter a ticket string"
            return
        }

        disableAllButtons()
        statusText.text = "Connecting..."

        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.connect(ticket)
            }

            if (handle == 0L) {
                statusText.text = "Connection failed"
                enableButtons()
                return@launch
            }

            sessionHandle = handle
            statusText.text = "Connected"
            disconnectButton.isEnabled = true

            startRenderLoop()
        }
    }

    private fun onDial() {
        val ticket = ticketInput.text.toString().trim()
        if (ticket.isEmpty()) {
            statusText.text = "Enter a call ticket"
            return
        }

        disableAllButtons()
        statusText.text = "Dialing..."

        // Start camera first so frames are ready when the encoder starts.
        startCamera()

        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.dial(ticket, CAMERA_WIDTH, CAMERA_HEIGHT)
            }

            if (handle == 0L) {
                statusText.text = "Call failed"
                enableButtons()
                stopCamera()
                return@launch
            }

            sessionHandle = handle
            statusText.text = "In call"
            disconnectButton.isEnabled = true

            // Start pushing camera frames to the Rust encoder.
            startCameraFramePush()

            startRenderLoop()
        }
    }

    private fun onDebugDirect() {
        disableAllButtons()
        statusText.text = "Direct..."
        startCamera()
        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.startDirect(CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            if (handle == 0L) {
                statusText.text = "Direct init failed"
                enableButtons()
                stopCamera()
                return@launch
            }
            sessionHandle = handle
            statusText.text = "Direct"
            disconnectButton.isEnabled = true
            awaitCameraAndPush()
            startRenderLoop()
        }
    }

    private fun onDebugH264() {
        disableAllButtons()
        statusText.text = "H264..."
        startCamera()
        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.startH264(CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            if (handle == 0L) {
                statusText.text = "H264 init failed"
                enableButtons()
                stopCamera()
                return@launch
            }
            sessionHandle = handle
            statusText.text = "H264"
            disconnectButton.isEnabled = true
            awaitCameraAndPush()
            startRenderLoop()
        }
    }

    private fun disableAllButtons() {
        connectButton.isEnabled = false
        dialButton.isEnabled = false
        directButton.isEnabled = false
        h264Button.isEnabled = false
        publishButton.isEnabled = false
        copyTicketButton.isEnabled = false
    }

    private fun enableButtons() {
        connectButton.isEnabled = true
        dialButton.isEnabled = true
        directButton.isEnabled = true
        h264Button.isEnabled = true
        publishButton.isEnabled = true
        copyTicketButton.isEnabled = false
        renditionSpinner.visibility = View.GONE
    }

    private fun onPublish() {
        val name = ticketInput.text.toString().trim().ifEmpty { "hello" }
        disableAllButtons()
        statusText.text = "Publishing..."
        startCamera()
        lifecycleScope.launch {
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.publish(name, CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            if (handle == 0L) {
                statusText.text = "Publish failed"
                enableButtons()
                stopCamera()
                return@launch
            }
            sessionHandle = handle
            val ticket = IrohBridge.getTicket(handle)
            statusText.text = "Publishing: $name"
            disconnectButton.isEnabled = true
            copyTicketButton.isEnabled = true
            awaitCameraAndPush()
        }
    }

    private fun onCopyTicket() {
        val handle = sessionHandle
        if (handle == 0L) return
        val ticket = IrohBridge.getTicket(handle)
        if (ticket.isEmpty()) return

        // Copy to clipboard.
        val clipboard = getSystemService(android.content.Context.CLIPBOARD_SERVICE) as android.content.ClipboardManager
        clipboard.setPrimaryClip(android.content.ClipData.newPlainText("iroh-live ticket", ticket))

        // Show QR code dialog.
        try {
            val qr = com.google.zxing.BarcodeFormat.QR_CODE
            val writer = com.google.zxing.MultiFormatWriter()
            val matrix = writer.encode(ticket, qr, 512, 512)
            val w = matrix.width
            val h = matrix.height
            val pixels = IntArray(w * h)
            for (y in 0 until h) {
                for (x in 0 until w) {
                    pixels[y * w + x] = if (matrix.get(x, y)) android.graphics.Color.BLACK else android.graphics.Color.WHITE
                }
            }
            val bitmap = android.graphics.Bitmap.createBitmap(pixels, w, h, android.graphics.Bitmap.Config.ARGB_8888)
            val imageView = android.widget.ImageView(this)
            imageView.setImageBitmap(bitmap)
            imageView.setPadding(32, 32, 32, 32)
            android.app.AlertDialog.Builder(this)
                .setTitle("Ticket copied")
                .setView(imageView)
                .setPositiveButton("OK", null)
                .show()
        } catch (e: Exception) {
            Log.w(TAG, "QR generation failed", e)
            android.widget.Toast.makeText(this, "Ticket copied", android.widget.Toast.LENGTH_SHORT).show()
        }
    }

    /** Waits for CameraX to be ready, then starts pushing frames to Rust. */
    private suspend fun awaitCameraAndPush() {
        // startCamera() runs its callback on the main executor; yield until
        // imageAnalysis is set (typically <200ms).
        while (imageAnalysis == null) {
            delay(50)
        }
        startCameraFramePush()
    }

    // ── Camera (CameraX) ────────────────────────────────────────────────

    private var imageAnalysis: ImageAnalysis? = null

    private fun startCamera() {
        val cameraProviderFuture = ProcessCameraProvider.getInstance(this)
        cameraProviderFuture.addListener({
            val provider = cameraProviderFuture.get()
            cameraProvider = provider

            val resolutionSelector = ResolutionSelector.Builder()
                .setResolutionStrategy(
                    ResolutionStrategy(
                        android.util.Size(CAMERA_WIDTH, CAMERA_HEIGHT),
                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER
                    )
                )
                .build()

            val preview = Preview.Builder()
                .setResolutionSelector(resolutionSelector)
                .build()
                .also { it.surfaceProvider = cameraPreview.surfaceProvider }

            val analysis = ImageAnalysis.Builder()
                .setResolutionSelector(resolutionSelector)
                .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            imageAnalysis = analysis

            try {
                provider.unbindAll()
                val camera = provider.bindToLifecycle(
                    this, CameraSelector.DEFAULT_FRONT_CAMERA, preview, analysis
                )
                sensorRotation = camera.cameraInfo.sensorRotationDegrees
                cameraPreview.visibility = View.VISIBLE
                Log.i(TAG, "CameraX started, sensorRotation=$sensorRotation")
            } catch (e: Exception) {
                Log.e(TAG, "CameraX bind failed", e)
            }
        }, ContextCompat.getMainExecutor(this))
    }

    private fun stopCamera() {
        cameraProvider?.unbindAll()
        cameraProvider = null
        imageAnalysis = null
        cameraPreview.visibility = View.GONE
    }

    /**
     * Pushes NV12 plane data from the ImageAnalysis pipeline directly to Rust.
     *
     * CameraX YUV_420_888 on Android typically has UV pixel stride 2, which is
     * NV12 (interleaved VU). We extract Y and UV planes and pass them straight
     * through — no CPU colorspace conversion needed.
     *
     * If the UV pixel stride is 1 (planar I420, rare on modern devices), we
     * interleave U+V into NV12 layout as a fallback.
     */
    private var nv12FrameCount = 0L

    private fun startCameraFramePush() {
        val analysis = imageAnalysis ?: return
        nv12FrameCount = 0
        val executor = java.util.concurrent.Executors.newSingleThreadExecutor()
        analysis.setAnalyzer(executor) { image ->
            val handle = sessionHandle
            if (handle != 0L && image.format == ImageFormat.YUV_420_888) {
                pushNv12(image, handle)
                nv12FrameCount++
                if (nv12FrameCount == 1L) {
                    Log.i(TAG, "First NV12 frame pushed: ${image.width}x${image.height} " +
                        "uvPixelStride=${image.planes[1].pixelStride} " +
                        "yStride=${image.planes[0].rowStride} " +
                        "uvStride=${image.planes[1].rowStride}")
                }
            }
            image.close()
        }
    }

    private fun pushNv12(image: ImageProxy, handle: Long) {
        val yPlane = image.planes[0]
        val uvPlane = image.planes[1] // U plane; on NV12 devices, interleaved UV
        val vPlane = image.planes[2]

        val width = image.width
        val height = image.height
        val yStride = yPlane.rowStride
        val uvStride = uvPlane.rowStride
        val uvPixelStride = uvPlane.pixelStride

        // Extract Y plane bytes (may include row padding).
        val yBuf = yPlane.buffer
        val ySize = yStride * height
        val yData = ByteArray(ySize)
        yBuf.position(0)
        yBuf.get(yData, 0, ySize.coerceAtMost(yBuf.remaining()))

        val uvHeight = height / 2

        if (uvPixelStride == 2) {
            // NV12/NV21: UV data is already interleaved. Read from the U plane
            // buffer which starts at the first U byte — gives UVUV... (NV12
            // order) that matches what Rust's NV12 path expects.
            val uvBuf = uvPlane.buffer
            val uvSize = uvStride * uvHeight
            val uvData = ByteArray(uvSize)
            uvBuf.position(0)
            uvBuf.get(uvData, 0, uvSize.coerceAtMost(uvBuf.remaining()))

            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, uvStride)
        } else {
            // Planar I420 (pixel stride 1): manually interleave U+V into NV12.
            val uBuf = uvPlane.buffer
            val vBuf = vPlane.buffer
            val uvWidth = width / 2
            val uvData = ByteArray(uvStride * uvHeight)
            for (row in 0 until uvHeight) {
                for (col in 0 until uvWidth) {
                    val srcIdx = row * uvPlane.rowStride + col
                    val dstIdx = row * uvStride + col * 2
                    uvData[dstIdx] = uBuf.get(srcIdx)
                    uvData[dstIdx + 1] = vBuf.get(srcIdx)
                }
            }
            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, width)
        }
    }

    private fun onDisconnect() {
        val handle = sessionHandle
        val job = renderJob
        renderJob = null

        stopCamera()

        disconnectButton.isEnabled = false
        statusText.text = "Disconnecting..."

        lifecycleScope.launch(Dispatchers.IO) {
            // Cancel the render loop BEFORE clearing the handle. The render
            // loop borrows the native session via borrow_handle(); if we set
            // sessionHandle to 0 and call disconnect (take_handle) while the
            // render loop still holds a reference, we get a use-after-free.
            job?.cancelAndJoin()
            sessionHandle = 0
            if (handle != 0L) {
                IrohBridge.disconnect(handle)
            }
            withContext(Dispatchers.Main) {
                enableButtons()
                statusText.text = "Disconnected"
            }
        }
    }

    // Current surface dimensions (updated from surfaceChanged).
    private var surfaceWidth = 0
    private var surfaceHeight = 0

    /**
     * Render loop: Rust owns the full EGL lifecycle and GL rendering.
     * Kotlin just polls renderNextFrame and updates the status UI.
     */
    private fun startRenderLoop() {
        renderJob = lifecycleScope.launch(Dispatchers.Default) {
            // Wait for surface to be ready.
            while (isActive && !surfaceReady) {
                delay(50L)
            }
            if (!isActive) return@launch

            // Rust creates the EGL context + GL renderer from the Surface.
            val handle = sessionHandle
            if (handle == 0L) return@launch
            IrohBridge.initSurface(handle, videoSurface.holder.surface)

            var statusCounter = 0L
            try {
                while (isActive) {
                    val h = sessionHandle
                    if (h == 0L) break

                    if (!surfaceReady) {
                        delay(50L)
                        continue
                    }

                    // Update status line and rendition list every ~30 frames.
                    statusCounter++
                    if (statusCounter % 30 == 0L) {
                        val line = IrohBridge.getStatusLine(h)
                        if (line.isNotEmpty()) {
                            runOnUiThread { statusText.text = line }
                        }
                        updateRenditionSpinner(h)
                    }

                    // Rust renders + swaps buffers internally.
                    if (!IrohBridge.renderNextFrame(
                            h, surfaceWidth, surfaceHeight, sensorRotation
                        )
                    ) {
                        delay(2L)
                    }
                }
            } finally {
                if (handle != 0L) {
                    IrohBridge.teardownSurface(handle)
                }
            }
        }
    }

    private var currentRenditions: List<String> = emptyList()
    private var renditionListenerActive = false

    private fun updateRenditionSpinner(handle: Long) {
        val raw = IrohBridge.getRenditions(handle)
        val names = if (raw.isBlank()) emptyList() else raw.split("\n")
        if (names == currentRenditions) return
        currentRenditions = names
        runOnUiThread {
            if (names.size <= 1) {
                renditionSpinner.visibility = View.GONE
                return@runOnUiThread
            }
            renditionSpinner.visibility = View.VISIBLE
            val adapter = android.widget.ArrayAdapter(
                this, android.R.layout.simple_spinner_item, names
            )
            adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item)
            renditionListenerActive = false
            renditionSpinner.adapter = adapter
            // Select current rendition if we have a video track.
            renditionSpinner.post {
                renditionListenerActive = true
                renditionSpinner.onItemSelectedListener = object : android.widget.AdapterView.OnItemSelectedListener {
                    override fun onItemSelected(parent: android.widget.AdapterView<*>?, view: View?, pos: Int, id: Long) {
                        if (!renditionListenerActive) return
                        val h = sessionHandle
                        if (h != 0L && pos < names.size) {
                            lifecycleScope.launch(Dispatchers.IO) {
                                IrohBridge.switchRendition(h, names[pos])
                            }
                        }
                    }
                    override fun onNothingSelected(parent: android.widget.AdapterView<*>?) {}
                }
            }
        }
    }

}
