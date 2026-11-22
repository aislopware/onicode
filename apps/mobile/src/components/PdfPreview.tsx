import { useGT } from "gt-react-native";
import { useState } from "react";
import { View } from "react-native";
import { WebView } from "react-native-webview";

import { FilePreviewLoading, FilePreviewNotice } from "../features/files/FilePreviewFeedback";
import { tryOpenExternalUrl } from "../lib/openExternalUrl";
import { pdfWebViewNavigation } from "../lib/pdfPreview";
import { webViewDecelerationRate } from "../lib/webViewDecelerationRate";

export function PdfPreview(props: { readonly uri: string }) {
  const gt = useGT();
  const [error, setError] = useState<string | null>(null);
  const [documentUrl, setDocumentUrl] = useState<string | null>(null);
  return (
    <View className="flex-1 bg-card">
      {error ? (
        <FilePreviewNotice title={gt("Could not load PDF")}>{error}</FilePreviewNotice>
      ) : null}
      <WebView
        source={{ uri: props.uri }}
        originWhitelist={["*"]}
        automaticallyAdjustContentInsets={false}
        decelerationRate={webViewDecelerationRate}
        contentInsetAdjustmentBehavior="never"
        startInLoadingState
        onShouldStartLoadWithRequest={(request) => {
          const navigation = pdfWebViewNavigation(request, documentUrl);
          if (navigation.kind === "open") void tryOpenExternalUrl(navigation.url, "file-preview");
          return navigation.kind === "load";
        }}
        onLoadEnd={(event) => setDocumentUrl(event.nativeEvent.url)}
        onError={(event) =>
          setError(event.nativeEvent.description || gt("The PDF could not be loaded."))
        }
        onHttpError={(event) =>
          setError(
            gt("The server returned HTTP {statusCode}.", {
              statusCode: event.nativeEvent.statusCode,
            }),
          )
        }
        renderLoading={() => <FilePreviewLoading message={gt("Loading PDF…")} background="card" />}
        style={{ flex: 1, backgroundColor: "transparent" }}
      />
    </View>
  );
}
