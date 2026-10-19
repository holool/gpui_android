package org.holool.gpui

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Rect
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.*
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeProvider
import android.view.inputmethod.*
import android.text.InputType
import android.widget.FrameLayout
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import org.json.JSONArray
import org.json.JSONObject

/** Reusable native host. A consumer only overrides nativeLibraryName(). */
open class GpuiActivity : Activity(), Choreographer.FrameCallback {
    protected open fun nativeLibraryName(): String = "gpui_minimal"
    private lateinit var view: GpuiView
    private val ui = Handler(Looper.getMainLooper())
    private var scheduled = false
    private var started = false
    private var resumed = false
    private var surfaceLive = false
    private var appHandlesBack = false
    private var configDirty = false
    private var lastSnapshot = ""
    private var backCallback: OnBackInvokedCallback? = null
    private val ime get() = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        System.loadLibrary(nativeLibraryName())
        window.setDecorFitsSystemWindows(false)
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)
        if (Build.VERSION.SDK_INT >= 33) { backCallback=OnBackInvokedCallback { routeBack() };onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, backCallback!!) }
        view = GpuiView(this)
        setContentView(FrameLayout(this).apply { addView(view, FrameLayout.LayoutParams(-1,-1)) })
        view.setOnApplyWindowInsetsListener { _, insets -> publishInsets(insets); insets }
        view.setWindowInsetsAnimationCallback(object : WindowInsetsAnimation.Callback(DISPATCH_MODE_CONTINUE_ON_SUBTREE) {
            override fun onProgress(insets: WindowInsets, animations: MutableList<WindowInsetsAnimation>): WindowInsets { publishInsets(insets);return insets }
        })
    }
    private fun publishInsets(insets: WindowInsets) {
        if (!started) return
        val safe = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
        val keyboard = insets.getInsets(WindowInsets.Type.ime())
        GpuiNative.insets(safe.left,safe.top,safe.right,safe.bottom,keyboard.left,keyboard.top,keyboard.right,keyboard.bottom)
    }
    internal fun created(surface: Surface,w: Int,h: Int) {
        val scale = resources.displayMetrics.density
        if (!started) {
            started = GpuiNative.start(this,surface,w,h,scale)
            check(started) { "GPUI startup failed; inspect adb logcat -s HoloolGPUI" }
        } else GpuiNative.surface(surface,w,h,scale)
        surfaceLive = true
        GpuiNative.lifecycle(if (resumed) 1 else 0)
        view.requestApplyInsets()
        wake()
    }
    internal fun changed(w: Int,h: Int) { if (started) GpuiNative.resize(w,h,resources.displayMetrics.density);wake() }
    internal fun destroyed() { surfaceLive=false;if(started) GpuiNative.detach() }
    override fun onStart(){super.onStart();if(started)GpuiNative.lifecycle(0)}
    override fun onResume(){super.onResume();resumed=true;if(started)GpuiNative.lifecycle(1);wake()}
    override fun onPause(){resumed=false;if(started)GpuiNative.lifecycle(2);super.onPause()}
    override fun onStop(){if(started)GpuiNative.lifecycle(3);Choreographer.getInstance().removeFrameCallback(this);scheduled=false;super.onStop()}
    override fun onDestroy(){if(Build.VERSION.SDK_INT>=33)backCallback?.let{onBackInvokedDispatcher.unregisterOnBackInvokedCallback(it)};if(started){GpuiNative.shutdown();started=false};super.onDestroy()}
    override fun onTrimMemory(level:Int){super.onTrimMemory(level);if(started)GpuiNative.memoryWarning()}
    /** Thread-safe wakeup; schedules one vsync rather than polling when idle. */
    fun wake(){if(Looper.myLooper()!=Looper.getMainLooper()){ui.post{wake()};return};if(!scheduled&&started&&surfaceLive&&resumed){scheduled=true;Choreographer.getInstance().postFrameCallback(this)}}
    override fun doFrame(frameTimeNanos:Long){scheduled=false;if(!started||!surfaceLive||!resumed)return;GpuiNative.frame();refreshInput()}
    internal fun refreshInput(){
        val value=GpuiNative.snapshot()
        if(value!=lastSnapshot){lastSnapshot=value;view.updateSnapshot(value)}
        if(configDirty){configDirty=false;ime.restartInput(view)}
    }
    fun inputConfigurationChanged(){configDirty=true;wake()}
    fun showKeyboard(){ui.post{view.requestFocus();ime.showSoftInput(view,InputMethodManager.SHOW_IMPLICIT)}}
    fun hideKeyboard(){ui.post{ime.hideSoftInputFromWindow(view.windowToken,0)}}
    fun backEnabled(value:Boolean){appHandlesBack=value}
    @Deprecated("Uses GPUI back routing and the system keyboard dismissal")
    override fun onBackPressed(){routeBack()}
    private fun routeBack(){if(view.rootWindowInsets?.isVisible(WindowInsets.Type.ime())==true){hideKeyboard()}else if(started&&appHandlesBack){GpuiNative.back()}else finish()}
    fun setWindowTitle(title:String){setTitle(title)}
    fun openUrl(url:String){ui.post{runCatching{startActivity(Intent(Intent.ACTION_VIEW,Uri.parse(url)))}}}
    fun setClipboardText(text:String){(getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("",text))}
    fun clipboardText():String?=(getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).primaryClip?.getItemAt(0)?.coerceToText(this)?.toString()
    fun moveToBackground(){moveTaskToBack(true)}
    fun accessibilityChanged(){ui.post{view.sendAccessibilityEvent(AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED)}}
}

internal class GpuiView(private val host:GpuiActivity):SurfaceView(host),SurfaceHolder.Callback {
    private var snapshot=JSONObject()
    private var connection:GpuiInputConnection?=null
    private val ime get()=context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
    init {isFocusable=true;isFocusableInTouchMode=true;importantForAccessibility=IMPORTANT_FOR_ACCESSIBILITY_YES;holder.addCallback(this)}
    override fun surfaceCreated(holder:SurfaceHolder){host.created(holder.surface,width.coerceAtLeast(1),height.coerceAtLeast(1))}
    override fun surfaceChanged(holder:SurfaceHolder,format:Int,width:Int,height:Int){host.changed(width,height)}
    override fun surfaceDestroyed(holder:SurfaceHolder){host.destroyed()}
    override fun onCheckIsTextEditor()=true
    override fun onCreateInputConnection(out:EditorInfo):InputConnection {
        val config=snapshot.optJSONObject("configuration")?:JSONObject()
        out.inputType=InputType.TYPE_CLASS_TEXT
        if(config.optString("action") in listOf("Enter","Unspecified",""))out.inputType=out.inputType or InputType.TYPE_TEXT_FLAG_MULTI_LINE
        if(config.optBoolean("autocorrect"))out.inputType=out.inputType or InputType.TYPE_TEXT_FLAG_AUTO_CORRECT
        if(!config.optBoolean("suggestions"))out.inputType=out.inputType or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        out.inputType=out.inputType or when(config.optString("capitalize")){"Words"->InputType.TYPE_TEXT_FLAG_CAP_WORDS;"Sentences"->InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;"Characters"->InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS;else->0}
        out.imeOptions=EditorInfo.IME_FLAG_NO_FULLSCREEN or when(config.optString("action")){"Done"->EditorInfo.IME_ACTION_DONE;"Go"->EditorInfo.IME_ACTION_GO;"Next"->EditorInfo.IME_ACTION_NEXT;"Previous"->EditorInfo.IME_ACTION_PREVIOUS;"Search"->EditorInfo.IME_ACTION_SEARCH;"Send"->EditorInfo.IME_ACTION_SEND;"Enter"->EditorInfo.IME_ACTION_NONE;else->EditorInfo.IME_ACTION_UNSPECIFIED}
        val selection=snapshot.optJSONObject("selection")
        out.initialSelStart=selection?.optInt("start")?:0;out.initialSelEnd=selection?.optInt("end")?:0
        if(android.os.Build.VERSION.SDK_INT>=30)out.setInitialSurroundingSubText(snapshot.optString("text"),snapshot.optInt("offset"))
        return GpuiInputConnection(this,host).also{connection=it}
    }
    fun updateSnapshot(value:String){
        val next=runCatching{JSONObject(value)}.getOrDefault(JSONObject())
        val previous=snapshot;snapshot=next
        val selection=next.optJSONObject("selection")?:JSONObject()
        val marked=next.optJSONObject("composing")
        ime.updateSelection(this,selection.optInt("start"),selection.optInt("end"),marked?.optInt("start")?:-1,marked?.optInt("end")?:-1)
        connection?.update(next)
        if(previous.optBoolean("enabled")!=next.optBoolean("enabled")&&next.optBoolean("enabled")){ime.restartInput(this);host.showKeyboard()}
    }
    fun snapshotValue():JSONObject=snapshot
    fun edit(command:JSONObject):Boolean {val result=GpuiNative.edit(command.toString());host.refreshInput();host.wake();return result}
    override fun onTouchEvent(event:MotionEvent):Boolean {
        requestFocus()
        val action=event.actionMasked
        if(action==MotionEvent.ACTION_CANCEL){GpuiNative.touch(3,0,0f,0f,0f)}
        else if(action==MotionEvent.ACTION_MOVE){for(i in 0 until event.pointerCount)GpuiNative.touch(2,event.getPointerId(i),event.getX(i),event.getY(i),event.getPressure(i))}
        else if(action==MotionEvent.ACTION_DOWN||action==MotionEvent.ACTION_POINTER_DOWN||action==MotionEvent.ACTION_UP||action==MotionEvent.ACTION_POINTER_UP){val i=event.actionIndex;GpuiNative.touch(if(action==MotionEvent.ACTION_DOWN||action==MotionEvent.ACTION_POINTER_DOWN)0 else 1,event.getPointerId(i),event.getX(i),event.getY(i),event.getPressure(i))}
        host.refreshInput();host.wake();return true
    }
    internal fun key(event:KeyEvent):Boolean {
        val names=mapOf(KeyEvent.KEYCODE_DEL to "backspace",KeyEvent.KEYCODE_FORWARD_DEL to "delete",KeyEvent.KEYCODE_ENTER to "enter",KeyEvent.KEYCODE_TAB to "tab",KeyEvent.KEYCODE_ESCAPE to "escape",KeyEvent.KEYCODE_DPAD_LEFT to "left",KeyEvent.KEYCODE_DPAD_RIGHT to "right",KeyEvent.KEYCODE_DPAD_UP to "up",KeyEvent.KEYCODE_DPAD_DOWN to "down",KeyEvent.KEYCODE_MOVE_HOME to "home",KeyEvent.KEYCODE_MOVE_END to "end")
        val name=names[event.keyCode]
        if(name==null&&event.action==KeyEvent.ACTION_DOWN&&event.unicodeChar!=0&&!event.isCtrlPressed&&!event.isAltPressed&&!event.isMetaPressed){return edit(JSONObject().put("op","commit").put("text",String(Character.toChars(event.unicodeChar))).put("cursor",1))}
        if(name!=null){GpuiNative.key(event.action==KeyEvent.ACTION_DOWN,name,event.isShiftPressed,event.isCtrlPressed,event.isAltPressed,event.isMetaPressed);host.refreshInput();return true}
        return false
    }
    override fun onKeyDown(code:Int,event:KeyEvent):Boolean=key(event)||super.onKeyDown(code,event)
    override fun onKeyUp(code:Int,event:KeyEvent):Boolean=key(event)||super.onKeyUp(code,event)
    // Android virtual node IDs are 32-bit. GPUI's 64-bit IDs must never be truncated.
    private val virtualIds=mutableMapOf<String,Int>()
    private val nativeIds=mutableMapOf<Int,String>()
    private var nextVirtualId=1
    private var accessibilityFocus=View.NO_ID
    private var hovered=View.NO_ID
    private fun virtualId(id:String):Int=virtualIds.getOrPut(id){val next=nextVirtualId++;nativeIds[next]=id;next}
    private fun nodes():Map<Int,JSONObject>{
        val data=JSONArray(GpuiNative.accessibility())
        return (0 until data.length()).map{data.getJSONObject(it)}.associateBy{virtualId(it.getString("id"))}
    }
    private fun event(id:Int,type:Int){
        val e=AccessibilityEvent.obtain(type)
        e.packageName=context.packageName;e.className="android.view.View";e.setSource(this,id)
        parent?.requestSendAccessibilityEvent(this,e)
    }
    private fun bounds(n:JSONObject):Rect{
        val b=n.optJSONArray("bounds")?:JSONArray();val s=resources.displayMetrics.density
        return Rect((b.optDouble(0)*s).toInt(),(b.optDouble(1)*s).toInt(),(b.optDouble(2)*s).toInt(),(b.optDouble(3)*s).toInt())
    }
    override fun dispatchHoverEvent(e:MotionEvent):Boolean{
        val manager=context.getSystemService(Context.ACCESSIBILITY_SERVICE) as android.view.accessibility.AccessibilityManager
        if(!manager.isTouchExplorationEnabled)return super.dispatchHoverEvent(e)
        val id=if(e.action==MotionEvent.ACTION_HOVER_EXIT)View.NO_ID else nodes().entries.lastOrNull{bounds(it.value).contains(e.x.toInt(),e.y.toInt())}?.key?:View.NO_ID
        if(id!=hovered){if(hovered!=View.NO_ID)event(hovered,AccessibilityEvent.TYPE_VIEW_HOVER_EXIT);hovered=id;if(id!=View.NO_ID)event(id,AccessibilityEvent.TYPE_VIEW_HOVER_ENTER)}
        return id!=View.NO_ID
    }
    private val provider=object:AccessibilityNodeProvider(){
        override fun createAccessibilityNodeInfo(id:Int):AccessibilityNodeInfo? {
            val data=nodes()
            if(id==View.NO_ID){return AccessibilityNodeInfo.obtain(this@GpuiView).apply{
                className="android.view.View";packageName=context.packageName;setSource(this@GpuiView)
                data.filterValues{it.optBoolean("root")}.keys.forEach{addChild(this@GpuiView,it)}
            }}
            val n=data[id]?:return null
            return AccessibilityNodeInfo.obtain().apply{
                packageName=context.packageName;setSource(this@GpuiView,id);isVisibleToUser=true;isEnabled=true
                val parentId=data.entries.firstOrNull{entry->val c=entry.value.optJSONArray("children")?:JSONArray();(0 until c.length()).any{c.getString(it)==n.getString("id")}}?.key
                if(parentId!=null)setParent(this@GpuiView,parentId)else setParent(this@GpuiView)
                className=when(n.optString("role")){"Button"->"android.widget.Button";"TextInput","MultilineTextInput"->"android.widget.EditText";"CheckBox"->"android.widget.CheckBox";else->"android.widget.TextView"}
                text=n.optString("value").takeUnless{it=="null"}?:n.optString("label").takeUnless{it=="null"}
                contentDescription=n.optString("label").takeUnless{it=="null"}
                val rect=bounds(n);val local=Rect(rect);parentId?.let{data[it]?.let{parentNode->val origin=bounds(parentNode);local.offset(-origin.left,-origin.top)}};setBoundsInParent(local);val position=IntArray(2);this@GpuiView.getLocationOnScreen(position);rect.offset(position[0],position[1]);setBoundsInScreen(rect)
                val children=n.optJSONArray("children")?:JSONArray();for(i in 0 until children.length())addChild(this@GpuiView,virtualId(children.getString(i)))
                if(n.optBoolean("click")){isClickable=true;addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_CLICK)}
                isFocusable=n.optBoolean("focus");isAccessibilityFocused=id==accessibilityFocus
                addAction(if(id==accessibilityFocus)AccessibilityNodeInfo.AccessibilityAction.ACTION_CLEAR_ACCESSIBILITY_FOCUS else AccessibilityNodeInfo.AccessibilityAction.ACTION_ACCESSIBILITY_FOCUS)
            }
        }
        override fun findFocus(focus:Int):AccessibilityNodeInfo?=if(focus==AccessibilityNodeInfo.FOCUS_ACCESSIBILITY&&accessibilityFocus!=View.NO_ID)createAccessibilityNodeInfo(accessibilityFocus)else null
        override fun performAction(id:Int,action:Int,args:Bundle?):Boolean{
            val nativeId=nativeIds[id]?:return false
            when(action){
                AccessibilityNodeInfo.ACTION_CLEAR_ACCESSIBILITY_FOCUS->{if(accessibilityFocus==id){accessibilityFocus=View.NO_ID;event(id,AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUS_CLEARED)};return true}
                AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS->{if(accessibilityFocus!=id){if(accessibilityFocus!=View.NO_ID)event(accessibilityFocus,AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUS_CLEARED);accessibilityFocus=id;event(id,AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUSED)}}
                AccessibilityNodeInfo.ACTION_CLICK->event(id,AccessibilityEvent.TYPE_VIEW_CLICKED)
                else->return false
            }
            GpuiNative.accessibilityAction(java.lang.Long.parseUnsignedLong(nativeId),action);host.wake();return true
        }
    }
    override fun getAccessibilityNodeProvider():AccessibilityNodeProvider=provider

}

internal class GpuiInputConnection(private val view:GpuiView,private val host:GpuiActivity):BaseInputConnection(view,true){
    private var state=view.snapshotValue()
    private var monitored:ExtractedTextRequest?=null
    private var batch=0
    fun update(value:JSONObject){state=value;monitored?.let{request->(view.context.getSystemService(Context.INPUT_METHOD_SERVICE)as InputMethodManager).updateExtractedText(view,request.token,extracted())}}
    private val text get()=state.optString("text")
    private val offset get()=state.optInt("offset")
    private val selection get()=state.optJSONObject("selection")?:JSONObject()
    private val start get()=(selection.optInt("start")-offset).coerceIn(0,text.length)
    private val end get()=(selection.optInt("end")-offset).coerceIn(start,text.length)
    private fun command(op:String)=JSONObject().put("op",op)
    override fun commitText(text:CharSequence?,cursor:Int)=view.edit(command("commit").put("text",text?.toString()?:"").put("cursor",cursor))
    override fun setComposingText(text:CharSequence?,cursor:Int)=view.edit(command("compose").put("text",text?.toString()?:"").put("cursor",cursor))
    override fun setComposingRegion(start:Int,end:Int)=view.edit(command("region").put("start",start).put("end",end))
    override fun finishComposingText()=view.edit(command("finish"))
    override fun setSelection(start:Int,end:Int)=view.edit(command("selection").put("start",start).put("end",end))
    override fun deleteSurroundingText(before:Int,after:Int)=view.edit(command("delete").put("before",before.coerceAtLeast(0)).put("after",after.coerceAtLeast(0)))
    override fun deleteSurroundingTextInCodePoints(before:Int,after:Int)=view.edit(command("deleteCodepoints").put("before",before.coerceAtLeast(0)).put("after",after.coerceAtLeast(0)))
    override fun getTextBeforeCursor(length:Int,flags:Int):CharSequence=text.substring((start-length.coerceAtLeast(0)).coerceAtLeast(0),start)
    override fun getTextAfterCursor(length:Int,flags:Int):CharSequence=text.substring(end,(end+length.coerceAtLeast(0)).coerceAtMost(text.length))
    override fun getSelectedText(flags:Int):CharSequence?=if(start==end)null else text.substring(start,end)
    override fun getCursorCapsMode(req:Int):Int=android.text.TextUtils.getCapsMode(text,start,req)
    private fun extracted()=ExtractedText().apply{this.text=this@GpuiInputConnection.text;startOffset=offset;selectionStart=start;selectionEnd=end;partialStartOffset=-1;partialEndOffset=-1}
    override fun getExtractedText(request:ExtractedTextRequest?,flags:Int):ExtractedText{if(flags and InputConnection.GET_EXTRACTED_TEXT_MONITOR!=0)monitored=request;return extracted()}
    override fun beginBatchEdit():Boolean{batch++;return true}
    override fun endBatchEdit():Boolean{batch=(batch-1).coerceAtLeast(0);if(batch==0)host.refreshInput();return true}
    override fun performEditorAction(action:Int):Boolean{GpuiNative.key(true,"enter",false,false,false,false);GpuiNative.key(false,"enter",false,false,false,false);if(action==EditorInfo.IME_ACTION_DONE)host.hideKeyboard();host.refreshInput();return true}
    override fun sendKeyEvent(event:KeyEvent)=view.key(event)
    override fun performContextMenuAction(id:Int):Boolean=when(id){android.R.id.selectAll->setSelection(offset,offset+text.length);android.R.id.copy->{host.setClipboardText(getSelectedText(0)?.toString()?:"");true};android.R.id.cut->{host.setClipboardText(getSelectedText(0)?.toString()?:"");commitText("",1)};android.R.id.paste->commitText(host.clipboardText()?:"",1);else->false}
    override fun closeConnection(){monitored=null;super.closeConnection()}
}

object GpuiNative {
    @JvmStatic external fun start(activity:Activity,surface:Surface,width:Int,height:Int,scale:Float):Boolean
    @JvmStatic external fun surface(surface:Surface,width:Int,height:Int,scale:Float)
    @JvmStatic external fun resize(width:Int,height:Int,scale:Float)
    @JvmStatic external fun detach()
    @JvmStatic external fun shutdown()
    @JvmStatic external fun frame()
    @JvmStatic external fun lifecycle(phase:Int)
    @JvmStatic external fun memoryWarning()
    @JvmStatic external fun touch(action:Int,id:Int,x:Float,y:Float,force:Float)
    @JvmStatic external fun insets(l:Int,t:Int,r:Int,b:Int,il:Int,it:Int,ir:Int,ib:Int)
    @JvmStatic external fun snapshot():String
    @JvmStatic external fun edit(command:String):Boolean
    @JvmStatic external fun key(down:Boolean,key:String,shift:Boolean,ctrl:Boolean,alt:Boolean,meta:Boolean)
    @JvmStatic external fun back()
    @JvmStatic external fun accessibility():String
    @JvmStatic external fun accessibilityAction(id:Long,action:Int)
}
