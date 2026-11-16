//! A short-lived QML overlay hosted by KWin, not a persistent desktop effect.
//! Commands arrive on our parent's private stdin; only KWin can consume them.
use super::*;
use std::{future::poll_fn, io::BufRead, pin::Pin};

const OBJECT: &str = "/dev/onicode/KdeCapture/Feedback";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    bounds: Bounds,
    pid: u32,
    flash: bool,
    animate: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum Event {
    Ready { animate: bool },
    Landed,
    Done,
}

struct Bridge {
    owner: String,
    commands: async_channel::Receiver<String>,
    done: async_channel::Sender<()>,
}
impl Bridge {
    fn authorize(&self, header: &zbus::message::Header<'_>) -> zbus::fdo::Result<()> {
        if header.sender().map(|name| name.as_str()) == Some(self.owner.as_str()) {
            Ok(())
        } else {
            Err(zbus::fdo::Error::AccessDenied("Not KWin".into()))
        }
    }
}
#[zbus::interface(name = "dev.onicode.KdeCapture.Feedback")]
impl Bridge {
    // An async pending call is an event channel, not a timer or a blocking KWin call.
    async fn next(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<String> {
        self.authorize(&header)?;
        Ok(self
            .commands
            .recv()
            .await
            .unwrap_or_else(|_| "{\"command\":\"close\"}".into()))
    }

    fn event(
        &self,
        value: &str,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.authorize(&header)?;
        let event: Event = serde_json::from_str(value)
            .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid feedback event".into()))?;
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{}", serde_json::to_string(&event).unwrap());
        let _ = stdout.flush();
        if matches!(event, Event::Done) {
            let _ = self.done.try_send(());
        }
        Ok(())
    }
}

pub(super) async fn supported(connection: &Connection) -> bool {
    let Ok(proxy) = Proxy::new(
        connection,
        "org.kde.KWin",
        "/Scripting",
        "org.freedesktop.DBus.Introspectable",
    )
    .await
    else {
        return false;
    };
    proxy
        .call::<_, _, String>("Introspect", &())
        .await
        .is_ok_and(|xml| xml.contains("name=\"loadDeclarativeScript\""))
}

pub(super) async fn run(connection: &Connection, directory: &Path, options: &str) -> Result<()> {
    let mut options: Options = serde_json::from_str(options)?;
    if options.bounds.width == 0 || options.bounds.height == 0 || options.pid == 0 {
        return Err("Invalid capture feedback geometry".into());
    }
    // Use KDE's config reader to respect config layering and the desktop's motion setting.
    let factor = std::process::Command::new("kreadconfig6")
        .args([
            "--file",
            "kdeglobals",
            "--group",
            "KDE",
            "--key",
            "AnimationDurationFactor",
            "--default",
            "1",
        ])
        .output()?;
    if !factor.status.success() {
        return Err("Couldn't read KDE animation preferences".into());
    }
    let factor: f64 = String::from_utf8(factor.stdout)?.trim().parse()?;
    if !factor.is_finite() || factor < 0.0 {
        return Err("Invalid KDE animation preference".into());
    }
    options.animate &= factor > 0.0;
    let owner = zbus::fdo::DBusProxy::new(connection)
        .await?
        .get_name_owner("org.kde.KWin".try_into()?)
        .await?
        .to_string();
    let (send, commands) = async_channel::bounded(4);
    let (done, finished) = async_channel::bounded(1);
    connection
        .object_server()
        .at(
            OBJECT,
            Bridge {
                owner,
                commands,
                done,
            },
        )
        .await?;

    // Subscribe before checking, so a lock racing setup can't expose the snapshot.
    let locker = Proxy::new(
        connection,
        "org.freedesktop.ScreenSaver",
        "/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    )
    .await?;
    let mut signals = locker.receive_signal("ActiveChanged").await?;
    if locker.call::<_, _, bool>("GetActive", &()).await? {
        return Err("Screen is locked".into());
    }
    let lock_commands = send.clone();
    let _lock_watch = zruntime::spawn(async move {
        while let Some(signal) =
            poll_fn(|cx| futures_core::Stream::poll_next(Pin::new(&mut signals), cx)).await
        {
            if signal
                .body()
                .deserialize::<(bool,)>()
                .is_ok_and(|(active,)| active)
            {
                let _ = lock_commands.send("{\"command\":\"close\"}".into()).await;
                break;
            }
        }
    });
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            let mut line = String::new();
            match input.by_ref().take(8193).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > 8192 => break,
                _ => {
                    if send.send_blocking(line).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = send.send_blocking("{\"command\":\"close\"}".into());
    });

    let path = directory.join("feedback.qml");
    let name = format!("oni-capture-feedback-{}", std::process::id());
    let bus = serde_json::to_string(
        connection
            .unique_name()
            .ok_or("Missing bus identity")?
            .as_str(),
    )?;
    let source = include_str!("feedback.qml")
        .replace("/*OPTIONS*/ null", &serde_json::to_string(&options)?)
        .replace("/*BUS*/ \"\"", &bus)
        .replace("/*NAME*/ \"\"", &serde_json::to_string(&name)?)
        .replace("/*FACTOR*/ 1", &factor.clamp(0.0, 3.0).to_string());
    std::fs::write(&path, source)?;
    std::fs::write(
        directory.join("feedbackGeometry.js"),
        include_str!("feedbackGeometry.js"),
    )?;
    let scripting = Proxy::new(
        connection,
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting",
    )
    .await?;
    let result = async {
        let id: i32 = scripting
            .call(
                "loadDeclarativeScript",
                &(path.to_str().ok_or("Invalid script path")?, &name),
            )
            .await?;
        if id < 0 {
            return Err("KWin could not load capture feedback".into());
        }
        let object = format!("/Scripting/Script{id}");
        let loaded = Proxy::new(
            connection,
            "org.kde.KWin",
            object.as_str(),
            "org.kde.kwin.Script",
        )
        .await?;
        let _: () = loaded.call("run", &()).await?;
        within(Duration::from_secs(12), finished.recv()).await??;
        Ok(())
    }
    .await;
    let _: zbus::Result<bool> = scripting.call("unloadScript", &(&name,)).await;
    let _ = connection.object_server().remove::<Bridge, _>(OBJECT).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kwin_can_read_commands_or_acknowledge_feedback() {
        zbus::block_on(async {
            let f = crate::transport_tests::Fixture::new().await;
            let (send, commands) = async_channel::bounded(4);
            let (done, finished) = async_channel::bounded(1);
            // Build with the bridge registered so zbus waits for its method-call listener.
            // Registering it after build can drop the first call while that listener starts.
            let bridge = Builder::address(f.address.as_str())
                .method_timeout(DEADLINE)
                .serve_at(
                    OBJECT,
                    Bridge {
                        owner: f._server.unique_name().unwrap().to_string(),
                        commands,
                        done,
                    },
                )
                .build()
                .await
                .unwrap();
            let destination = bridge.unique_name().unwrap().to_string();
            let stranger = Builder::address(f.address.as_str())
                .method_timeout(DEADLINE)
                .build()
                .await
                .unwrap();
            let unrelated = Proxy::new(
                &stranger,
                destination.as_str(),
                OBJECT,
                "dev.onicode.KdeCapture.Feedback",
            )
            .await
            .unwrap();
            let error = unrelated
                .call::<_, _, String>("Next", &())
                .await
                .unwrap_err();
            assert!(
                matches!(&error, zbus::Error::FDO(error) if matches!(**error, zbus::fdo::Error::AccessDenied(_))),
                "{error:?}"
            );
            assert!(
                unrelated
                    .call::<_, _, ()>("Event", &("{\"event\":\"done\"}",))
                    .await
                    .is_err()
            );
            assert!(finished.try_recv().is_err());

            let kwin = Proxy::new(
                &f._server,
                destination.clone(),
                OBJECT,
                "dev.onicode.KdeCapture.Feedback",
            )
            .await
            .unwrap();
            // Event remains responsive while Next waits. No blocking the D-Bus executor.
            let next = kwin.clone();
            let request =
                zruntime::spawn(
                    async move { next.call::<_, _, String>("Next", &()).await.unwrap() },
                );
            kwin.call::<_, _, ()>("Event", &("{\"event\":\"ready\",\"animate\":true}",))
                .await
                .unwrap();
            send.send("{\"command\":\"close\"}".into()).await.unwrap();
            assert_eq!(request.await.unwrap(), "{\"command\":\"close\"}");
            kwin.call::<_, _, ()>("Event", &("{\"event\":\"done\"}",))
                .await
                .unwrap();
            within(DEADLINE, finished.recv()).await.unwrap().unwrap();
            bridge
                .object_server()
                .remove::<Bridge, _>(OBJECT)
                .await
                .unwrap();
        });
    }

    #[test]
    fn capability_probe_rejects_missing_declarative_api_without_running_a_script() {
        zbus::block_on(async {
            let f = crate::transport_tests::Fixture::new().await;
            assert!(!supported(&f.client).await);
        });
    }
}
