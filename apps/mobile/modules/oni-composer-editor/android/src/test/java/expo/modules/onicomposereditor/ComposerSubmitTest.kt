package expo.modules.onicomposereditor

import android.text.InputType
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.InputDeviceBuilder

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], manifest = Config.NONE)
class ComposerSubmitTest {
  private val sends = mutableListOf<Boolean>()
  private val editor = SelectionAwareEditText(RuntimeEnvironment.getApplication()).apply {
    inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
    submitEnabled = true
    submitListener = { alternate -> sends += alternate }
  }

  private fun keyboard(id: Int, type: Int = InputDevice.KEYBOARD_TYPE_ALPHABETIC): InputDevice =
    InputDeviceBuilder.newBuilder()
      .setId(id)
      .setSources(InputDevice.SOURCE_KEYBOARD)
      .setKeyboardType(type)
      .build()

  private fun press(
    keyCode: Int,
    metaState: Int = 0,
    device: InputDevice = keyboard(1),
    repeatCount: Int = 0,
    flags: Int = 0
  ) {
    val event = KeyEvent(
      0,
      0,
      KeyEvent.ACTION_DOWN,
      keyCode,
      repeatCount,
      metaState,
      device.id,
      0,
      flags,
    )
    // Robolectric's getDevice() reads the event's shadow, not the InputManager device list.
    shadowOf(event).setDevice(device)
    editor.onKeyDown(keyCode, event)
  }

  private fun pressReturn(
    metaState: Int = 0,
    device: InputDevice = keyboard(1),
    repeatCount: Int = 0,
    flags: Int = 0
  ) = press(KeyEvent.KEYCODE_ENTER, metaState, device, repeatCount, flags)

  @Test
  fun sendBehaviorSendsOnReturnAndInsertsANewlineOnShiftReturn() {
    pressReturn()
    pressReturn(KeyEvent.META_CTRL_ON or KeyEvent.META_CTRL_LEFT_ON)
    pressReturn(KeyEvent.META_SHIFT_ON or KeyEvent.META_SHIFT_LEFT_ON)

    assertEquals(listOf(false, true), sends)
    assertEquals("\n", editor.text.toString())
  }

  @Test
  fun newlineBehaviorSendsOnlyWithCtrl() {
    editor.returnSends = false

    pressReturn()
    pressReturn(KeyEvent.META_CTRL_ON)
    pressReturn(KeyEvent.META_CTRL_ON or KeyEvent.META_SHIFT_ON)

    assertEquals(listOf(false, true), sends)
    assertEquals("\n", editor.text.toString())
  }

  @Test
  fun onScreenKeyboardReturnInsertsANewline() {
    pressReturn(
      device = keyboard(KeyCharacterMap.VIRTUAL_KEYBOARD),
      flags = KeyEvent.FLAG_SOFT_KEYBOARD
    )
    // Some IMEs send device 0, the phone's key-only built-in "keyboard", without FLAG_SOFT_KEYBOARD.
    pressReturn(device = keyboard(0, InputDevice.KEYBOARD_TYPE_NON_ALPHABETIC))

    assertEquals(emptyList<Boolean>(), sends)
    assertEquals("\n\n", editor.text.toString())
  }

  @Test
  fun heldReturnSendsOnce() {
    pressReturn()
    pressReturn(repeatCount = 1)
    pressReturn(repeatCount = 2)

    assertEquals(listOf(false), sends)
    assertEquals("", editor.text.toString())
  }

  @Test
  fun returnNeverSendsFromAComposerThatCannotSubmit() {
    editor.submitEnabled = false
    pressReturn()
    editor.submitEnabled = true

    editor.readOnly = true
    pressReturn()

    assertEquals(emptyList<Boolean>(), sends)
  }

  @Test
  fun arrowsRecallAPromptOnlyFromTheFirstAndLastLine() {
    val recalls = mutableListOf<Boolean>()
    editor.promptRecallListener = { older -> recalls += older }
    editor.setText("first line\nsecond line")
    editor.layoutParams = ViewGroup.LayoutParams(
      ViewGroup.LayoutParams.MATCH_PARENT,
      ViewGroup.LayoutParams.WRAP_CONTENT,
    )
    editor.measure(
      View.MeasureSpec.makeMeasureSpec(1_000, View.MeasureSpec.EXACTLY),
      View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
    )
    editor.layout(0, 0, 1_000, editor.measuredHeight)
    val firstLine = 3
    val lastLine = 14

    editor.setSelection(firstLine)
    press(KeyEvent.KEYCODE_DPAD_UP)
    assertEquals("recall is off until JS enables it", emptyList<Boolean>(), recalls)

    editor.promptRecallEnabled = true
    press(KeyEvent.KEYCODE_DPAD_UP)
    editor.setSelection(firstLine)
    press(KeyEvent.KEYCODE_DPAD_DOWN)
    editor.setSelection(lastLine)
    press(KeyEvent.KEYCODE_DPAD_UP)
    editor.setSelection(lastLine)
    press(KeyEvent.KEYCODE_DPAD_DOWN)
    assertEquals(listOf(true, false), recalls)

    press(KeyEvent.KEYCODE_DPAD_DOWN, metaState = KeyEvent.META_SHIFT_ON)
    editor.setSelection(lastLine)
    press(
      KeyEvent.KEYCODE_DPAD_DOWN,
      device = keyboard(0, InputDevice.KEYBOARD_TYPE_NON_ALPHABETIC),
    )
    editor.setSelection(firstLine, lastLine)
    press(KeyEvent.KEYCODE_DPAD_DOWN)
    assertEquals("other arrows move the caret", listOf(true, false), recalls)
  }
}
