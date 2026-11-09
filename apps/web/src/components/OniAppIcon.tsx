// The production icon, as GitHub shows the Onicode app, whatever this build's channel.
import appIconUrl from "../../../../assets/prod/oni-black-web-apple-touch-180.png";

export function OniAppIcon(props: { readonly className?: string }) {
  return <img src={appIconUrl} alt="" className={props.className} />;
}
