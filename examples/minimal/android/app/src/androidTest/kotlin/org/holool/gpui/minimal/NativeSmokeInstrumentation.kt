package org.holool.gpui.minimal

import android.app.Activity
import android.app.Instrumentation
import android.content.Intent
import android.os.Bundle
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import org.holool.gpui.GpuiActivity
import org.holool.gpui.GpuiNative
import org.json.JSONObject

/** No test framework dependency: exercises the real Activity -> JNI -> GPUI input path. */
class NativeSmokeInstrumentation : Instrumentation() {
    override fun onCreate(arguments: Bundle?) { super.onCreate(arguments); start() }
    override fun onStart() {
        try {
            val activity = startActivitySync(Intent(targetContext, GpuiActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            val deadline = System.currentTimeMillis() + 30_000
            var ready = false
            while (!ready && System.currentTimeMillis() < deadline) {
                runOnMainSync { ready = JSONObject(GpuiNative.snapshot()).optBoolean("enabled") }
                if (!ready) Thread.sleep(100)
            }
            check(ready) { "GPUI input handler was not installed" }
            runOnMainSync {
                fun surface(view: View): SurfaceView? = if (view is SurfaceView) view else if(view is ViewGroup) (0 until view.childCount).firstNotNullOfOrNull { surface(view.getChildAt(it)) } else null
                val view = checkNotNull(surface(activity.window.decorView))
                val connection = checkNotNull(view.onCreateInputConnection(EditorInfo()))
                fun state() = JSONObject(GpuiNative.snapshot())
                fun checkText(value: String) { check(state().getString("text") == value) { state().toString() } }
                connection.setSelection(0, state().getString("text").length)
                check(connection.commitText("x😀سلام🦀y", 1))
                connection.setSelection(5,5)
                connection.setComposingRegion(3,7)
                check(connection.deleteSurroundingTextInCodePoints(1,1))
                checkText("xسلامy")
                check(state().getJSONObject("composing").getInt("start") == 1)
                check(state().getJSONObject("composing").getInt("end") == 5)
                check(state().getJSONObject("selection").getInt("start") == 3)
                check(connection.setComposingText("أهلاً😀",1))
                checkText("xأهلاً😀y")
                check(connection.commitText("مرحبا",0))
                checkText("xمرحباy")
                check(state().getJSONObject("selection").getInt("start") == 1)
                check(state().isNull("composing"))
                connection.setSelection(1,6)
                check(connection.getSelectedText(0).toString() == "مرحبا")
                check(connection.getTextBeforeCursor(1,0).toString() == "x")
                check(connection.getTextAfterCursor(1,0).toString() == "y")
                connection.deleteSurroundingText(1,1)
                checkText("مرحبا")
                check(state().getJSONObject("selection").getInt("start") == 0)
                check(state().getJSONObject("selection").getInt("end") == 5)
                connection.setSelection(0,0)
                connection.commitText("😀",1)
                connection.deleteSurroundingTextInCodePoints(1,0)
                checkText("مرحبا")
                connection.finishComposingText()
                connection.setComposingText("س",1)
                GpuiNative.back()
                check(state().isNull("composing")) { "Application Back did not consume composition" }
                check(!activity.isFinishing)
                connection.closeConnection()
                GpuiNative.back()
                check(activity.isFinishing) { "Unconsumed Back did not finish Activity" }
            }
            finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "GPUI native InputConnection smoke passed: Arabic composition, UTF16 selection, both-side/codepoint deletion, consumed/unconsumed Back\n") })
        } catch (error: Throwable) {
            finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", error.stackTraceToString()) })
        }
    }
}
