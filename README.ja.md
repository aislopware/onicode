# Zeron

コーディングエージェント（Claude Code、Codex、Cursor、Devin、Grok、Hermes、Pi、Antigravity）をデフォルトではローカルで管理し、必要に応じて複数デバイス間で同期することもできます。

*[English](README.md) | [简体中文](README.zh-CN.md) | [한국어](README.ko.md) | 日本語 | [Español](README.es.md)*

![Zeron デスクトップアプリ](docs/media/readme/app-screenshot.jpg)

## デスクトップアプリ

[GitHub Releases](https://github.com/zeronsh/zeron/releases/latest) から、お使いのプラットフォーム向けの最新版をダウンロードしてください。

- **macOS** — `zeron-<version>-macos-arm64.dmg`
- **Linux** — `zeron-<version>-linux-<arch>.tar.gz` を展開し、中の `install.sh` を実行

アカウントもネットワーク接続も不要で、セッションはお使いのデバイスに保存されます。アプリは自動でアップデートされます。

## ヘッドレス実行（CLI）

サーバーなど、ディスプレイのないマシン向けです。たとえば、ノートパソコンを閉じたあともエージェントを動かし続ける VPS に使えます。Linux のみ対応しています。

```bash
./install.sh      # Linux 版リリースの tarball に同梱
zeron status
```

インストーラーはエンジンをバックグラウンドサービスとして起動し、再起動後も自動的に立ち上がります。

```bash
zeron status      # エンジンの状態を確認
zeron update      # 最新版にアップデート
zeron daemon start|stop|restart|status
```

## 複数デバイス

あるデバイスで起動したエージェントを別のデバイスから確認したり操作したりできます。エンジンを動かしている tailnet 上のデバイスは、サインインも中継サーバーもなしで互いを見つけます。各デバイスは互いのワークスペースのファイルを読み書きできるので、エンジンは信頼できるデバイスでのみ動かしてください。

## スポンサー

Zeron をスポンサーしてくださっている [The Context Company](https://www.thecontextcompany.com/) に感謝します。[GitHub でスポンサーになって](https://github.com/sponsors/zeronsh) Zeron の開発を支援することもできます。

---

開発に参加したい方や仕組みが気になる方は、[Ask DeepWiki](https://deepwiki.com/zeronsh/zeron) または [ARCHITECTURE.md](ARCHITECTURE.md) をご覧ください。

[MIT License](LICENSE) のもとで公開されています。
