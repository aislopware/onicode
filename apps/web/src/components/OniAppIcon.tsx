// The production icon, as GitHub shows the Onicode app, whatever this build's channel.
import appIconUrl from "../../../../assets/prod/oni-black-splash-logo-180.png";
import { cn } from "~/lib/utils";

export function OniAppIcon(props: { readonly className?: string }) {
  return <img src={appIconUrl} alt="" className={cn("rounded-sm", props.className)} />;
}
