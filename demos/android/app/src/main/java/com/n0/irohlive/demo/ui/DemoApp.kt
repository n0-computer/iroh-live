package com.n0.irohlive.demo.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import com.n0.irohlive.demo.Screen
import com.n0.irohlive.demo.SessionController

/**
 * Routes between the screens and shows the controller's one-shot notices.
 *
 * The full-screen video and publish screens draw under the system bars, so the
 * scaffold contributes no insets and each screen decides for itself whether to
 * pad.
 */
@Composable
fun DemoApp(controller: SessionController) {
    val snackbar = remember { SnackbarHostState() }
    val notice = controller.notice
    LaunchedEffect(notice) {
        if (notice != null) {
            snackbar.showSnackbar(notice)
            controller.notice = null
        }
    }

    BackHandler(enabled = controller.screen != Screen.Home) { controller.back() }

    IrohLiveTheme {
        Scaffold(
            snackbarHost = { SnackbarHost(snackbar) },
            containerColor = MaterialTheme.colorScheme.background,
            contentWindowInsets = WindowInsets(0, 0, 0, 0),
        ) { _ ->
            Box(Modifier.fillMaxSize()) {
                when (val screen = controller.screen) {
                    Screen.Home -> HomeScreen(
                        onWatch = controller::openWatchSetup,
                        onPublish = controller::openPublishSetup,
                        onCall = controller::openCallSetup,
                        onLoopback = controller::loopback,
                        onDenied = { controller.notice = "Camera access is needed for the loopback pipelines" },
                        modifier = Modifier.safeDrawingPadding(),
                    )

                    Screen.WatchSetup -> WatchSetupScreen(
                        ticket = controller.ticketDraft,
                        busy = controller.busy,
                        onTicketChange = { controller.ticketDraft = it },
                        onWatch = controller::watch,
                        onBack = controller::openHome,
                        onDenied = { controller.notice = "Camera access is needed to scan" },
                        modifier = Modifier.safeDrawingPadding(),
                    )

                    Screen.CallSetup -> CallSetupScreen(
                        ticket = controller.ticketDraft,
                        busy = controller.busy,
                        onTicketChange = { controller.ticketDraft = it },
                        onCall = controller::call,
                        onWait = controller::waitForCall,
                        onBack = controller::openHome,
                        onDenied = {
                            controller.notice = "Camera and microphone access are needed to call"
                        },
                        modifier = Modifier.safeDrawingPadding(),
                    )

                    Screen.PublishSetup -> PublishSetupScreen(
                        name = controller.nameDraft,
                        busy = controller.busy,
                        onNameChange = { controller.nameDraft = it },
                        onPublish = controller::publish,
                        onBack = controller::openHome,
                        onDenied = {
                            controller.notice = "Camera and microphone access are needed to publish"
                        },
                        modifier = Modifier.safeDrawingPadding(),
                    )

                    is Screen.Watch -> WatchPlayer(
                        title = screen.title,
                        status = controller.status,
                        renditions = controller.renditions,
                        target = controller.renderTarget,
                        onSelectRendition = controller::selectRendition,
                        onExit = controller::stop,
                    )

                    is Screen.Call -> CallScreen(
                        ticket = screen.ticket,
                        status = controller.status,
                        target = controller.renderTarget,
                        onCopy = controller::copyTicket,
                        onStop = controller::stop,
                    )

                    is Screen.Publish -> PublishLiveScreen(
                        name = screen.name,
                        ticket = screen.ticket,
                        onCopy = controller::copyTicket,
                        onStop = controller::stop,
                        onPreviewView = { controller.previewView = it },
                    )
                }
            }
        }
    }
}
