# Zeron

Controla tus agentes de programación (Claude Code, Codex, Cursor, Devin, Grok, Hermes, Pi, Antigravity) de forma local por defecto, con sincronización opcional entre varios dispositivos.

*[English](README.md) | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | [日本語](README.ja.md) | Español*

![App de escritorio de Zeron](docs/media/readme/app-screenshot.jpg)

## App de escritorio

Descarga la última versión para tu plataforma desde [GitHub Releases](https://github.com/zeronsh/zeron/releases/latest):

- **macOS** — `zeron-<version>-macos-arm64.dmg`
- **Linux** — `zeron-<version>-linux-<arch>.tar.gz` y luego ejecuta el `install.sh` que incluye

No necesitas cuenta ni conexión a internet; las sesiones se quedan en tu dispositivo. La app se actualiza sola.

## Modo headless (CLI)

Para servidores y otras máquinas sin pantalla, como un VPS que mantiene los agentes en ejecución después de cerrar tu portátil. Solo Linux:

```bash
./install.sh      # del tarball de Linux de la versión
zeron status
```

El instalador arranca el motor como un servicio en segundo plano que sobrevive a los reinicios.

```bash
zeron status      # estado del motor
zeron update      # actualizar a la última versión
zeron daemon start|stop|restart|status
```

## Varios dispositivos

Lanza un agente en un dispositivo y síguelo o contrólalo desde otro: cada dispositivo de tu tailnet que ejecuta el motor encuentra a los demás por sí solo, sin iniciar sesión y sin servidor intermedio. Cada dispositivo puede leer y escribir los archivos del espacio de trabajo de los demás, así que ejecuta el motor solo en dispositivos de confianza.

## Patrocinadores

Gracias a [The Context Company](https://www.thecontextcompany.com/) por patrocinar Zeron. Tú también puedes ayudar a financiar el desarrollo de Zeron [convirtiéndote en patrocinador en GitHub](https://github.com/sponsors/zeronsh).

---

¿Quieres contribuir o te interesa saber cómo funciona? [Pregúntale a DeepWiki](https://deepwiki.com/zeronsh/zeron) o consulta [ARCHITECTURE.md](ARCHITECTURE.md).

Publicado bajo la [licencia MIT](LICENSE).
