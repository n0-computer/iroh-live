package com.n0.irohlive.demo

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.LaunchedEffect
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import com.n0.irohlive.demo.ui.DemoApp

/**
 * The demo's only activity.
 *
 * It owns the [SessionController], keeps the window in step with the screen the
 * controller is showing, and turns `iroh-live:` links into a watch session.
 */
class MainActivity : ComponentActivity() {
    private lateinit var controller: SessionController

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        controller = SessionController(this, this, lifecycleScope)
        handleTicketLink(intent)

        setContent {
            val screen = controller.screen
            LaunchedEffect(screen) { applyWindowMode(screen) }
            DemoApp(controller)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleTicketLink(intent)
    }

    override fun onDestroy() {
        controller.dispose()
        super.onDestroy()
    }

    /**
     * Hides the system bars while video is on screen and keeps the display
     * awake for as long as a session is running.
     */
    private fun applyWindowMode(screen: Screen) {
        val playing = screen is Screen.Watch
        val session = playing || screen is Screen.Publish
        val insets = WindowInsetsControllerCompat(window, window.decorView)
        if (playing) {
            insets.systemBarsBehavior =
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            insets.hide(WindowInsetsCompat.Type.systemBars())
        } else {
            insets.show(WindowInsetsCompat.Type.systemBars())
        }
        if (session) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        } else {
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    /** Starts watching when the app was opened from an `iroh-live:` link. */
    private fun handleTicketLink(intent: Intent?) {
        val uri: Uri = intent?.data ?: return
        if (uri.scheme != "iroh-live") return
        val ticket = uri.toString()
        controller.ticketDraft = ticket
        controller.openWatchSetup()
        controller.watch(ticket)
    }
}
