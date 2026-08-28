use futures::channel::mpsc;
use jni::objects::{JObject, JString};
use std::sync::Mutex;

#[derive(Debug)]
pub(crate) struct VoicePrompt {
    pub thread_id: String,
    pub text: String,
}

static EVENT_TX: Mutex<Option<mpsc::UnboundedSender<VoicePrompt>>> = Mutex::new(None);

pub(crate) fn init_event_channel() -> mpsc::UnboundedReceiver<VoicePrompt> {
    let (tx, rx) = mpsc::unbounded();
    *EVENT_TX.lock().unwrap() = Some(tx);
    rx
}

pub(crate) fn drain(
    rx: &mut mpsc::UnboundedReceiver<VoicePrompt>,
    callback: &mut Option<Box<dyn FnMut(String, String)>>,
) {
    while let Ok(prompt) = rx.try_recv() {
        if let Some(callback) = callback.as_mut() {
            callback(prompt.thread_id, prompt.text);
        } else {
            log::warn!("voice prompt dropped because no application handler is registered");
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_zdroid_NativeBridge_nativeSubmitVoicePrompt<'local>(
    mut env: jni::JNIEnv<'local>,
    _bridge: JObject<'local>,
    thread_id: JString<'local>,
    text: JString<'local>,
) -> jni::sys::jboolean {
    let thread_id: String = match env.get_string(&thread_id) {
        Ok(value) => value.into(),
        Err(_) => return 0,
    };
    let text: String = match env.get_string(&text) {
        Ok(value) => value.into(),
        Err(_) => return 0,
    };
    if thread_id.trim().is_empty() || text.trim().is_empty() {
        return 0;
    }
    let Some(tx) = EVENT_TX.lock().unwrap().as_ref().cloned() else {
        log::warn!("voice prompt arrived before Android platform initialization");
        return 0;
    };
    match tx.unbounded_send(VoicePrompt { thread_id, text }) {
        Ok(()) => 1,
        Err(error) => {
            log::warn!("failed to queue voice prompt: {error:#}");
            0
        }
    }
}
