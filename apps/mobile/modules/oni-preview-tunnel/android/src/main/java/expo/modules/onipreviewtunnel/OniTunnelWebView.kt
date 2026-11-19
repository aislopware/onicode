package expo.modules.onipreviewtunnel

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.os.Handler
import android.os.Looper
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.webkit.ProfileStore
import androidx.webkit.ProxyConfig
import androidx.webkit.ProxyController
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.viewevent.EventDispatcher
import expo.modules.kotlin.views.ExpoView
import kotlin.math.abs

/**
 * A webview whose loopback is another environment's: `localhost` requests go to a SOCKS listener
 * in this process, which carries each connection over the environment's tunnel.
 *
 * Android's proxy override applies to every WebView in the app, so it carries loopback only while a
 * tunnel view is on screen and refuses it while one lives off screen (see [TunnelProxy]). Each
 * environment gets its own WebView profile, so one environment's localhost cookies and storage
 * never reach another's.
 */
@SuppressLint("SetJavaScriptEnabled")
class OniTunnelWebView(context: Context, appContext: AppContext) : ExpoView(context, appContext) {
  override val shouldUseAndroidLayout = true

  private val onAccessRequest by EventDispatcher()
  private val onNavigationChange by EventDispatcher()
  private val onLoadError by EventDispatcher()
  private val onPageScroll by EventDispatcher()
  private val main = Handler(Looper.getMainLooper())
  private var webView: WebView? = null
  private var proxied = false
  private var pendingUrl: String? = null
  private var loading = false
  internal var environmentId: String? = null
    private set
  private var reportedScrollY = 0f

  fun setEnvironment(environmentId: String) {
    if (environmentId == this.environmentId) return
    // A different environment never reuses this WebView or its profile.
    destroyWebView()
    this.environmentId = environmentId
    val missing = when {
      !WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE) -> "multi-profile"
      !WebViewFeature.isFeatureSupported(WebViewFeature.PROXY_OVERRIDE) -> "proxy-override"
      else -> null
    }
    if (missing != null) {
      unsupported(missing)
      return
    }
    val profile = "oni-preview-tunnel-$environmentId"
    ProfileStore.getInstance().getOrCreateProfile(profile)
    val webView = WebView(context)
    // The profile must be set before anything else touches this WebView.
    WebViewCompat.setProfile(webView, profile)
    webView.settings.javaScriptEnabled = true
    webView.settings.domStorageEnabled = true
    webView.webViewClient = object : WebViewClient() {
      override fun onPageStarted(view: WebView, url: String?, favicon: Bitmap?) = report(true)
      override fun onPageFinished(view: WebView, url: String?) = report(false)
      override fun doUpdateVisitedHistory(view: WebView, url: String?, isReload: Boolean) =
        report(null)

      override fun onReceivedError(
        view: WebView,
        request: WebResourceRequest,
        error: WebResourceError
      ) {
        if (request.isForMainFrame) fail(error.description.toString(), request.url.toString())
      }
    }
    webView.webChromeClient = object : WebChromeClient() {
      override fun onReceivedTitle(view: WebView, title: String?) = report(null)
    }
    webView.setOnScrollChangeListener { _, _, scrollY, _, _ -> reportScroll(scrollY) }
    addView(webView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
    this.webView = webView
    TunnelProxy.add(this)
    if (isAttachedToWindow) TunnelProxy.show(this)
  }

  private fun fail(description: String, url: String) {
    // A loopback connection refused because another environment's page is open says so.
    val refused = TunnelProxy.mixed && android.net.Uri.parse(url).host?.loopbackHost() != null
    val cause = if (refused) "conflict" else ""
    onLoadError(mapOf("code" to -1, "description" to description, "url" to url, "cause" to cause))
  }

  /** JavaScript explains it in the user's language; `feature` names what the WebView lacks. */
  internal fun unsupported(feature: String) {
    // Posted, so it reaches JavaScript once the view is mounted.
    post {
      onLoadError(mapOf("code" to -1, "description" to feature, "url" to "", "unsupported" to true))
    }
  }

  /** Reports the page's scroll position in steps, enough for the bar to follow its direction. */
  private fun reportScroll(scrollY: Int) {
    val y = scrollY / resources.displayMetrics.density
    val moved = abs(y - reportedScrollY) >= SCROLL_STEP_DP
    if (!moved && !(y == 0f && reportedScrollY != 0f)) return
    reportedScrollY = y
    onPageScroll(mapOf("y" to y))
  }

  private fun report(nowLoading: Boolean?) {
    val webView = webView ?: return
    if (nowLoading != null) loading = nowLoading
    onNavigationChange(
      mapOf(
        "url" to (webView.url ?: ""),
        "title" to (webView.title ?: ""),
        "loading" to loading,
        "canGoBack" to webView.canGoBack(),
        "canGoForward" to webView.canGoForward()
      )
    )
  }

  override fun onAttachedToWindow() {
    super.onAttachedToWindow()
    TunnelProxy.show(this)
  }

  override fun onDetachedFromWindow() {
    super.onDetachedFromWindow()
    TunnelProxy.hide(this)
  }

  internal fun onProxyReady() {
    proxied = true
    pendingUrl?.let { loadUrl(it) }
  }

  internal fun onProxyChanging() {
    proxied = false
  }

  /** Whether a page loaded, so another environment's opening leaves it be. */
  internal val hasPage: Boolean
    get() = webView?.url != null

  /** Opened while another environment's page is: it waits, unloaded, until that one closes. */
  internal fun onConflict() {
    post {
      if (!TunnelProxy.mixed) return@post
      val url = pendingUrl ?: ""
      onLoadError(mapOf("code" to -1, "description" to "", "url" to url, "cause" to "conflict"))
    }
  }

  internal fun requestAccess(requestId: String) {
    onAccessRequest(mapOf("requestId" to requestId))
  }

  /** Called when React removes the view; a detach alone keeps the page for a reattach. */
  fun destroyWebView() {
    webView?.let {
      removeView(it)
      it.destroy()
    }
    webView = null
    proxied = false
    TunnelProxy.remove(this)
  }

  /** Opens `url`, even when it is the address already showing. */
  fun loadUrl(url: String) {
    val webView = webView
    if (!proxied || webView == null) {
      pendingUrl = url
      if (TunnelProxy.mixed) onConflict()
      return
    }
    pendingUrl = null
    webView.loadUrl(url)
  }

  fun goBack() = main.post { webView?.goBack() }
  fun goForward() = main.post { webView?.goForward() }
  fun reload() = main.post {
    // Unproxied, the page waits for the override like any load, and says why while it can't come.
    if (proxied) webView?.reload() else (pendingUrl ?: webView?.url)?.let { loadUrl(it) }
  }

  private companion object {
    const val SCROLL_STEP_DP = 24f
  }
}

/**
 * The process-wide proxy override tunnel views share, set while one lives. A view in a screen
 * under another keeps running its page, and a connection can't name the view it came from, so each
 * listener serves one environment and refuses while pages of two are open; a view opened then
 * waits, unloaded, until only one environment's remain. While none is on screen the listener
 * refuses everything, so neither webviews of other screens nor a hidden page reach a loopback.
 * Another environment gets a new listener on a new port, which a connection meant for the old one
 * can never reach. Main thread only.
 */
internal object TunnelProxy {
  private val main = Handler(Looper.getMainLooper())

  /** Views with a page, from WebView creation until destroyed, shown or not. */
  private val views = mutableListOf<OniTunnelWebView>()
  private val shown = mutableSetOf<OniTunnelWebView>()
  private var listener: SocksListener? = null
  private var environmentId: String? = null
  private var applied = false

  /** Whether pages of more than one environment are open, so every connection is refused. */
  val mixed: Boolean
    get() = views.any { it.environmentId != views.first().environmentId }

  fun add(view: OniTunnelWebView) {
    views.remove(view)
    views.add(view)
    update()
  }

  fun remove(view: OniTunnelWebView) {
    views.remove(view)
    shown.remove(view)
    update()
  }

  fun show(view: OniTunnelWebView) {
    if (view !in views) return
    shown.add(view)
    update()
    if (mixed && !view.hasPage) view.onConflict()
  }

  fun hide(view: OniTunnelWebView) {
    if (shown.remove(view)) update()
  }

  private fun update() {
    // Null while mixed or none is on screen: that listener answers nothing.
    val environment = views.firstOrNull()?.environmentId.takeUnless { mixed || shown.isEmpty() }
    when {
      views.isEmpty() -> clear()
      listener == null || environment != environmentId -> route(environment)
      applied && environment != null -> for (view in views.toList()) view.onProxyReady()
    }
  }

  private fun clear() {
    if (listener != null) ProxyController.getInstance().clearProxyOverride({ it.run() }) {}
    close()
  }

  private fun close() {
    listener?.close()
    listener = null
    environmentId = null
    applied = false
    for (view in views) view.onProxyChanging()
  }

  /** Points the override at a new listener that answers only for `environment`'s pages. */
  private fun route(environment: String?) {
    close()
    val listener = SocksListener {
      TunnelAccess.request { requestId ->
        main.post {
          val answering = views.lastOrNull { it in shown } ?: views.lastOrNull()
          if (answering == null || views.any { it.environmentId != environment }) {
            TunnelAccess.resolve(requestId, null)
          } else {
            answering.requestAccess(requestId)
          }
        }
      }
    }
    val onSet = Runnable {
      main.post {
        // Replaced, or cleared, before this override applied.
        if (this.listener !== listener) return@post
        applied = true
        if (environmentId != null) for (view in views.toList()) view.onProxyReady()
      }
    }
    // `*` loads everything directly, and the implicit-rule removal after it takes loopback back
    // out of that bypass, the same split the desktop app makes with `*,<-loopback>`.
    // Older WebViews reject `socks5://`; their `socks://` is SOCKS4, which the listener speaks too.
    val set = listOf("socks5", "socks").any { scheme ->
      val config = ProxyConfig.Builder()
        .addProxyRule("$scheme://127.0.0.1:${listener.port}")
        .addBypassRule("*")
        .removeImplicitRules()
        .build()
      runCatching {
        ProxyController.getInstance().setProxyOverride(config, { it.run() }, onSet)
      }.isSuccess
    }
    if (set) {
      this.listener = listener
      environmentId = environment
    } else {
      listener.close()
      runCatching { ProxyController.getInstance().clearProxyOverride({ it.run() }) {} }
      for (view in views) view.unsupported("proxy-override")
    }
  }
}
