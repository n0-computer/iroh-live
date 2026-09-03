package com.n0.irohlive.demo

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.ImageFormat
import android.util.Log
import android.view.Surface
import android.view.SurfaceHolder
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.content.ContextCompat
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/** Which screen the app is showing. */
sealed interface Screen {
    /** The two modes, side by side. */
    data object Home : Screen

    /** Ticket entry and the QR scanner, before watching. */
    data object WatchSetup : Screen

    /** Ticket entry and the QR scanner, before calling, or a way to be called. */
    data object CallSetup : Screen

    /** Broadcast name entry, before going live. */
    data object PublishSetup : Screen

    /**
     * Video, full screen.
     *
     * [title] names what is playing, for the overlay that appears on tap.
     * [fromLocalCamera] is set for the loopback pipelines, whose frames come
     * out of the camera sideways and need the sensor rotation applied.
     */
    data class Watch(val title: String, val fromLocalCamera: Boolean) : Screen

    /**
     * A live broadcast, showing its ticket as a QR code.
     *
     * [ticket] is null until the native side has an endpoint to put in it.
     */
    data class Publish(val name: String, val ticket: String?) : Screen

    /**
     * A two-way call.
     *
     * [ticket] is set only while waiting to be called, and is the code the
     * other end scans. It clears when a peer arrives, which is also when the
     * picture stops being this node's own camera and becomes the peer's.
     */
    data class Call(val ticket: String?) : Screen
}

/**
 * The Android surface the native render loop draws into.
 *
 * The render loop runs off the main thread and the surface comes and goes with
 * the view, so every field is read across threads.
 */
class RenderTarget {
    @Volatile
    var holder: SurfaceHolder? = null

    @Volatile
    var width: Int = 0

    @Volatile
    var height: Int = 0
}

/**
 * Owns the native session, the camera, and the render loop.
 *
 * The composables read the observable properties and call the actions; nothing
 * about the UI reaches into JNI directly.
 */
class SessionController(
    private val context: Context,
    private val lifecycleOwner: LifecycleOwner,
    private val scope: CoroutineScope,
) {
    companion object {
        private const val TAG = "IrohLiveDemo"

        /** What the encoder is configured for, and what CameraX is asked for. */
        const val CAMERA_WIDTH = 1280
        const val CAMERA_HEIGHT = 720

        /** How long to wait for the camera preview view before publishing anyway. */
        private const val PREVIEW_WAIT_MILLIS = 1500L

        /** How often to ask the native side whether a caller has arrived. */
        private const val PEER_POLL_MS = 500L
    }

    var screen by mutableStateOf<Screen>(Screen.Home)
        private set

    /** The status line the native side reports, shown in the watch overlay. */
    var status by mutableStateOf("")
        private set

    /** Set while connecting or publishing, so the UI can show progress. */
    var busy by mutableStateOf(false)
        private set

    /** A one-shot message for the snackbar: a failure, or a copy confirmation. */
    var notice by mutableStateOf<String?>(null)

    /** Video renditions the broadcast offers, empty unless there is a choice. */
    var renditions by mutableStateOf<List<String>>(emptyList())
        private set

    /** The ticket the watch screen starts from, seeded by an `iroh-live:` intent. */
    var ticketDraft by mutableStateOf("")

    /** The name a published broadcast is reachable under. */
    var nameDraft by mutableStateOf("hello")

    val renderTarget = RenderTarget()

    /** Set by the publish screen so CameraX has somewhere to draw the preview. */
    @Volatile
    var previewView: PreviewView? = null

    @Volatile
    private var sessionHandle: Long = 0
    private var renderJob: Job? = null
    private var cameraProvider: ProcessCameraProvider? = null
    private var imageAnalysis: ImageAnalysis? = null
    private var cameraExecutor: ExecutorService? = null

    /** The camera sensor rotation in degrees, 0/90/180/270. */
    @Volatile
    private var sensorRotation = 0

    // -- navigation ---------------------------------------------------

    fun openHome() {
        screen = Screen.Home
    }

    fun openWatchSetup() {
        screen = Screen.WatchSetup
    }

    fun openPublishSetup() {
        screen = Screen.PublishSetup
    }

    fun openCallSetup() {
        screen = Screen.CallSetup
    }

    /** Handles the system back gesture. Returns false when there is nothing to go back to. */
    fun back(): Boolean = when (screen) {
        Screen.Home -> false
        Screen.WatchSetup, Screen.PublishSetup, Screen.CallSetup -> {
            openHome()
            true
        }
        is Screen.Watch, is Screen.Publish, is Screen.Call -> {
            stop()
            true
        }
    }

    // -- actions ------------------------------------------------------

    /** Subscribes to a remote broadcast and plays it full screen. */
    fun watch(ticket: String) {
        val trimmed = ticket.trim()
        if (trimmed.isEmpty()) {
            notice = "Enter or scan a ticket first"
            return
        }
        ticketDraft = trimmed
        busy = true
        scope.launch {
            val handle = withContext(Dispatchers.IO) { IrohBridge.connect(trimmed) }
            busy = false
            if (handle == 0L) {
                notice = "Could not connect to that ticket"
                return@launch
            }
            sessionHandle = handle
            status = "Connected"
            screen = Screen.Watch(title = "Watching", fromLocalCamera = false)
            startRenderLoop(rotateWithSensor = false)
        }
    }

    /** Publishes the camera and microphone, then shows the ticket as a QR code. */
    fun publish(name: String) {
        val broadcastName = name.trim().ifEmpty { "hello" }
        nameDraft = broadcastName
        busy = true
        screen = Screen.Publish(broadcastName, ticket = null)
        scope.launch {
            awaitPreviewView()
            startCamera()
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.publish(broadcastName, CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            busy = false
            if (handle == 0L) {
                stopCamera()
                screen = Screen.PublishSetup
                notice = "Could not start the broadcast"
                return@launch
            }
            sessionHandle = handle
            val ticket = withContext(Dispatchers.IO) { IrohBridge.getTicket(handle) }
            screen = Screen.Publish(broadcastName, ticket.ifEmpty { null })
            awaitCameraAndPush()
        }
    }

    /** Dials a peer, sending this camera and microphone and playing theirs. */
    fun call(ticket: String) {
        val trimmed = ticket.trim()
        if (trimmed.isEmpty()) {
            notice = "Enter or scan a ticket first"
            return
        }
        ticketDraft = trimmed
        busy = true
        screen = Screen.Call(ticket = null)
        scope.launch {
            // No `awaitPreviewView` here, unlike publishing: the call screens
            // draw through the native renderer, so CameraX has no preview use
            // case to bind and the analysis path that feeds the encoder does
            // not need one.
            startCamera()
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.dial(trimmed, CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            busy = false
            if (handle == 0L) {
                stopCamera()
                screen = Screen.CallSetup
                notice = "Could not reach that peer"
                return@launch
            }
            sessionHandle = handle
            status = "In a call"
            awaitCameraAndPush()
            startRenderLoop(rotateWithSensor = false)
        }
    }

    /**
     * Publishes this node's side of a call and shows the code to be dialed.
     *
     * The camera starts here rather than when a peer arrives, so the preview is
     * live behind the code and the first frame the caller sees does not wait
     * for a device to open. That preview is what the render loop draws until
     * the native side reports a peer, at which point the same loop starts
     * drawing the peer's pictures instead: the frame source is swapped under
     * it rather than the loop being restarted.
     */
    fun waitForCall() {
        busy = true
        screen = Screen.Call(ticket = null)
        scope.launch {
            startCamera()
            val handle = withContext(Dispatchers.IO) {
                IrohBridge.answer(CAMERA_WIDTH, CAMERA_HEIGHT)
            }
            busy = false
            if (handle == 0L) {
                stopCamera()
                screen = Screen.CallSetup
                notice = "Could not open an endpoint to be called on"
                return@launch
            }
            sessionHandle = handle
            val ticket = withContext(Dispatchers.IO) { IrohBridge.getTicket(handle) }
            screen = Screen.Call(ticket = ticket.ifEmpty { null })
            status = "Waiting for a call"
            awaitCameraAndPush()
            startRenderLoop(rotateWithSensor = true)
            awaitPeer(handle)
        }
    }

    /**
     * Polls the native side until a caller arrives, then takes the code down.
     *
     * Polled rather than pushed because everything else Kotlin asks the bridge
     * is a blocking call it makes itself; a callback would be the only inbound
     * edge in the whole interface, and a second of latency on a screen somebody
     * is holding up to a camera costs nothing.
     */
    private suspend fun awaitPeer(handle: Long) {
        while (sessionHandle == handle) {
            if (withContext(Dispatchers.IO) { IrohBridge.callConnected(handle) }) {
                screen = Screen.Call(ticket = null)
                status = "In a call"
                // The peer's pictures arrive the right way up; only this node's
                // own camera needs the sensor rotation applied.
                restartRenderLoop(rotateWithSensor = false)
                return
            }
            delay(PEER_POLL_MS)
        }
    }

    /**
     * Runs one of the offline pipelines, which never touch the network.
     *
     * [encoded] picks between raw camera passthrough and a local H.264
     * encode-decode round trip. Both are diagnostics: they tell you whether the
     * camera, the codec, and the renderer work before you blame the transport.
     */
    fun loopback(encoded: Boolean) {
        busy = true
        screen = Screen.Watch(
            title = if (encoded) "H.264 loopback" else "Camera passthrough",
            fromLocalCamera = true,
        )
        scope.launch {
            startCamera()
            val handle = withContext(Dispatchers.IO) {
                if (encoded) {
                    IrohBridge.startH264(CAMERA_WIDTH, CAMERA_HEIGHT)
                } else {
                    IrohBridge.startDirect(CAMERA_WIDTH, CAMERA_HEIGHT)
                }
            }
            busy = false
            if (handle == 0L) {
                stopCamera()
                screen = Screen.Home
                notice = "Could not start the loopback pipeline"
                return@launch
            }
            sessionHandle = handle
            awaitCameraAndPush()
            startRenderLoop(rotateWithSensor = true)
        }
    }

    /** Tears the session down and returns to the home screen. */
    fun stop() {
        val handle = sessionHandle
        val job = renderJob
        renderJob = null
        stopCamera()
        status = ""
        renditions = emptyList()
        screen = Screen.Home
        scope.launch(Dispatchers.IO) {
            // The render loop borrows the native session, so it has to be off
            // the handle before `disconnect` frees it.
            job?.cancelAndJoin()
            sessionHandle = 0
            if (handle != 0L) {
                IrohBridge.disconnect(handle)
            }
        }
    }

    /** Switches the video track being decoded. */
    fun selectRendition(name: String) {
        val handle = sessionHandle
        if (handle == 0L) return
        scope.launch(Dispatchers.IO) { IrohBridge.switchRendition(handle, name) }
    }

    /** Puts the current publish ticket on the clipboard. */
    fun copyTicket(ticket: String) {
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("iroh-live ticket", ticket))
        notice = "Ticket copied"
    }

    /** Frees everything the activity owns. Call from `onDestroy`. */
    fun dispose() {
        val job = renderJob
        renderJob = null
        if (job != null) {
            runBlocking { job.cancelAndJoin() }
        }
        stopCamera()
        val handle = sessionHandle
        sessionHandle = 0
        if (handle != 0L) {
            IrohBridge.disconnect(handle)
        }
    }

    // -- render loop --------------------------------------------------

    /**
     * Polls the native renderer, which owns the EGL context and draws straight
     * into the surface. Kotlin only supplies the surface and its size.
     *
     * The loop gets a thread of its own rather than a slice of
     * `Dispatchers.Default`. An EGL context belongs to the thread it was made
     * current on, and every `delay` in the loop is a point where a pooled
     * dispatcher could resume the coroutine somewhere else; on the emulator's
     * EGL that ends in a segfault inside `eglSwapBuffers`.
     */
    /**
     * Stops the render loop and starts it again with a different rotation.
     *
     * Answering a call is the one transition where what is being drawn changes
     * under a loop that is already running: this node's own camera up to the
     * moment a peer arrives, and the peer's pictures after it. The rotation the
     * loop applies is fixed when it starts, so the loop restarts rather than
     * being taught to re-read it every frame.
     */
    private suspend fun restartRenderLoop(rotateWithSensor: Boolean) {
        renderJob?.cancelAndJoin()
        renderJob = null
        startRenderLoop(rotateWithSensor)
    }

    private fun startRenderLoop(rotateWithSensor: Boolean) {
        val dispatcher = Executors.newSingleThreadExecutor { runnable ->
            Thread(runnable, "iroh-live-render")
        }.asCoroutineDispatcher()
        val job = scope.launch(dispatcher) {
            val handle = sessionHandle
            if (handle == 0L) return@launch

            // The surface goes away when the app is backgrounded and comes
            // back as a new one, so the loop attaches to whichever surface is
            // current rather than to the one it started with.
            var attached: Surface? = null
            var frames = 0L
            try {
                while (isActive && sessionHandle == handle) {
                    val surface = renderTarget.holder?.surface?.takeIf { it.isValid }
                    if (surface == null) {
                        if (attached != null) {
                            IrohBridge.teardownSurface(handle)
                            attached = null
                        }
                        delay(50L)
                        continue
                    }
                    if (surface !== attached) {
                        if (attached != null) IrohBridge.teardownSurface(handle)
                        IrohBridge.initSurface(handle, surface)
                        attached = surface
                        Log.i(TAG, "render surface attached")
                    }

                    frames++
                    if (frames % 30 == 0L) {
                        refreshStatus(handle)
                    }

                    val rotation = if (rotateWithSensor) sensorRotation else 0
                    val drew = IrohBridge.renderNextFrame(
                        handle, renderTarget.width, renderTarget.height, rotation
                    )
                    if (!drew) delay(2L)
                }
            } finally {
                if (attached != null) {
                    IrohBridge.teardownSurface(handle)
                }
            }
        }
        renderJob = job
        job.invokeOnCompletion { dispatcher.close() }
    }

    private suspend fun refreshStatus(handle: Long) {
        val line = IrohBridge.getStatusLine(handle)
        val raw = IrohBridge.getRenditions(handle)
        val names = if (raw.isBlank()) emptyList() else raw.split("\n")
        withContext(Dispatchers.Main) {
            if (line.isNotEmpty()) status = line
            if (names != renditions) renditions = names
        }
    }

    // -- camera -------------------------------------------------------

    private suspend fun awaitPreviewView() {
        var waited = 0L
        while (previewView == null && waited < PREVIEW_WAIT_MILLIS) {
            delay(50L)
            waited += 50L
        }
    }

    private suspend fun awaitCameraAndPush() {
        // `startCamera` binds from a listener on the main executor, so the
        // analysis use case appears a moment after the call returns.
        var waited = 0L
        while (imageAnalysis == null && waited < PREVIEW_WAIT_MILLIS) {
            delay(50L)
            waited += 50L
        }
        startCameraFramePush()
    }

    private fun startCamera() {
        val future = ProcessCameraProvider.getInstance(context)
        future.addListener({
            val provider = try {
                future.get()
            } catch (e: Exception) {
                Log.e(TAG, "no camera provider", e)
                notice = "No camera available"
                return@addListener
            }
            cameraProvider = provider

            val resolution = ResolutionSelector.Builder()
                .setResolutionStrategy(
                    ResolutionStrategy(
                        android.util.Size(CAMERA_WIDTH, CAMERA_HEIGHT),
                        ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER
                    )
                )
                .build()

            val analysis = ImageAnalysis.Builder()
                .setResolutionSelector(resolution)
                .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()

            val preview = previewView?.let { view ->
                Preview.Builder()
                    .setResolutionSelector(resolution)
                    .build()
                    .also { it.surfaceProvider = view.surfaceProvider }
            }

            try {
                provider.unbindAll()
                val useCases = listOfNotNull(preview, analysis).toTypedArray()
                val camera = provider.bindToLifecycle(
                    lifecycleOwner, CameraSelector.DEFAULT_FRONT_CAMERA, *useCases
                )
                imageAnalysis = analysis
                sensorRotation = camera.cameraInfo.sensorRotationDegrees
                Log.i(TAG, "camera bound, sensorRotation=$sensorRotation preview=${preview != null}")
            } catch (e: Exception) {
                Log.e(TAG, "camera bind failed", e)
                notice = "Camera unavailable: ${e.message}"
            }
        }, ContextCompat.getMainExecutor(context))
    }

    private fun stopCamera() {
        imageAnalysis?.clearAnalyzer()
        cameraProvider?.unbindAll()
        cameraProvider = null
        imageAnalysis = null
        cameraExecutor?.shutdown()
        cameraExecutor = null
    }

    private fun startCameraFramePush() {
        val analysis = imageAnalysis ?: return
        val executor = Executors.newSingleThreadExecutor()
        cameraExecutor = executor
        var pushed = 0L
        analysis.setAnalyzer(executor) { image ->
            val handle = sessionHandle
            if (handle != 0L && image.format == ImageFormat.YUV_420_888) {
                if (pushed == 0L) {
                    // Logged before the first push, not after, so a frame
                    // geometry the conversion cannot handle still says what it
                    // was.
                    Log.i(
                        TAG,
                        "first camera frame: ${image.width}x${image.height} " +
                            "uvPixelStride=${image.planes[1].pixelStride} " +
                            "yStride=${image.planes[0].rowStride} " +
                            "uvStride=${image.planes[1].rowStride}"
                    )
                }
                pushNv12(image, handle)
                pushed++
            }
            image.close()
        }
    }

    /**
     * Hands one camera frame to the native encoder as NV12 planes.
     *
     * CameraX YUV_420_888 has a UV pixel stride of 2 on most hardware, which is
     * NV12 already, so the planes go straight through with no colour space
     * conversion. Pixel stride 1 means planar I420, and those devices pay for a
     * manual interleave.
     */
    private fun pushNv12(image: ImageProxy, handle: Long) {
        val yPlane = image.planes[0]
        val uvPlane = image.planes[1]
        val vPlane = image.planes[2]

        val width = image.width
        val height = image.height
        val yStride = yPlane.rowStride
        val uvStride = uvPlane.rowStride

        val yBuf = yPlane.buffer
        val ySize = yStride * height
        val yData = ByteArray(ySize)
        yBuf.position(0)
        yBuf.get(yData, 0, ySize.coerceAtMost(yBuf.remaining()))

        val uvHeight = height / 2

        if (uvPlane.pixelStride == 2) {
            val uvBuf = uvPlane.buffer
            val uvSize = uvStride * uvHeight
            val uvData = ByteArray(uvSize)
            uvBuf.position(0)
            uvBuf.get(uvData, 0, uvSize.coerceAtMost(uvBuf.remaining()))
            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, uvStride)
        } else {
            val uBuf = uvPlane.buffer
            val vBuf = vPlane.buffer
            val uvWidth = width / 2
            // The interleaved plane has two bytes per chroma sample, so its
            // stride is the frame width, not the stride of the separate source
            // planes those samples are read from.
            val uvData = ByteArray(width * uvHeight)
            for (row in 0 until uvHeight) {
                val uRow = row * uvPlane.rowStride
                val vRow = row * vPlane.rowStride
                val dstRow = row * width
                for (col in 0 until uvWidth) {
                    uvData[dstRow + col * 2] = uBuf.get(uRow + col * uvPlane.pixelStride)
                    uvData[dstRow + col * 2 + 1] = vBuf.get(vRow + col * vPlane.pixelStride)
                }
            }
            IrohBridge.pushCameraNv12(handle, yData, uvData, width, height, yStride, width)
        }
    }
}
