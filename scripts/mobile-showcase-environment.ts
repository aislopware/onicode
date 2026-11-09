// @effect-diagnostics nodeBuiltinImport:off globalTimers:off globalDate:off - This host-side fixture creates an isolated local Onicode environment.
import * as NodeChildProcess from "node:child_process";
import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";
import * as NodeSqlite from "node:sqlite";
import * as NodeUtil from "node:util";

const execFile = NodeUtil.promisify(NodeChildProcess.execFile);

const SHOWCASE_PROJECT_ID = "onicode";
const SHOWCASE_THREAD_ID = "remote-command-center";

export const SHOWCASE_SCENES = [
  "threads",
  "thread",
  "terminal",
  "review",
  "environments",
  "agent-activity",
] as const;
export type ShowcaseScene = (typeof SHOWCASE_SCENES)[number];

const MODEL_SELECTION = JSON.stringify({ instanceId: "codex", model: "gpt-5.4" });
const PROJECT_SCRIPTS = JSON.stringify([
  {
    id: "dev",
    name: "Dev",
    command: "pnpm dev",
    icon: "play",
    runOnWorktreeCreate: false,
  },
  {
    id: "test",
    name: "Tests",
    command: "pnpm test",
    icon: "test",
    runOnWorktreeCreate: false,
  },
]);

const SHOWCASE_TERMINAL_PROMPT =
  "\u001b[1;32m→\u001b[0m \u001b[1;36monicode\u001b[0m \u001b[1;34mgit:(\u001b[1;31mfeat/remote-command-center\u001b[1;34m)\u001b[0m \u001b[1;33m✗\u001b[0m ";

const SHOWCASE_TERMINAL_BUFFER = [
  `${SHOWCASE_TERMINAL_PROMPT}vpr dev`,
  "",
  "  \u001b[1;32mVITE\u001b[0m \u001b[32mv7.1.1\u001b[0m  \u001b[2mready in\u001b[0m \u001b[1m1.24s\u001b[0m",
  "",
  "  \u001b[32m→\u001b[0m  \u001b[2mLocal:\u001b[0m    \u001b[4;36mhttp://127.0.0.1:5173/\u001b[0m",
  "  \u001b[32m→\u001b[0m  \u001b[2mNetwork:\u001b[0m  \u001b[4;36mhttp://192.168.1.24:5173/\u001b[0m",
  "  \u001b[32m→\u001b[0m  \u001b[2mProject:\u001b[0m  \u001b[1monicode\u001b[0m \u001b[2m(~/Code/onicode)\u001b[0m",
  "",
  "  \u001b[32m✓ 612 passed\u001b[0m   \u001b[33m△ 2 warnings\u001b[0m   \u001b[31m✗ 0 failed\u001b[0m",
  "",
  "  \u001b[42;30m READY \u001b[0m \u001b[2mwatching for changes, press\u001b[0m \u001b[1mq\u001b[0m \u001b[2mto quit\u001b[0m",
  "",
  SHOWCASE_TERMINAL_PROMPT,
].join("\r\n");

const BASE_ENVIRONMENT_PRESENCE = `export function environmentLabel(count: number): string {
  return \`${"${count}"} environments\`;
}
`;

const UPDATED_ENVIRONMENT_PRESENCE = `const PULSE = ["✦", "✧", "·", "✧"] as const;

export function environmentLabel(connected: number, total: number, frame: number): string {
  const pulse = PULSE[frame % PULSE.length];
  return \`${"${pulse} ${connected}/${total}"} ready\`;
}
`;

const REMOTE_HANDOFF_CARD = `import { View, Text } from "react-native";

export function RemoteHandoffCard(props: { machine: string; latencyMs: number }) {
  return (
    <View className="rounded-2xl bg-surface-2 p-4">
      <Text className="font-semibold">Ready on {props.machine}</Text>
      <Text className="text-success">Handoff in {props.latencyMs}ms</Text>
    </View>
  );
}
`;

const PROJECT_FAVICONS = {
  onicode: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
<defs><linearGradient id="onicode-body" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="#FBE6EC"/></linearGradient>
<filter id="onicode-tile-shadow" x="-10%" y="-10%" width="120%" height="125%"><feGaussianBlur stdDeviation="14"/></filter>
<filter id="onicode-char-shadow" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="12"/></filter>
</defs>
<path d="M824.0,412.0 L824.0,455.1 L824.0,471.9 L823.9,484.7 L823.9,495.4 L823.8,504.7 L823.7,513.1 L823.6,520.8 L823.5,527.9 L823.4,534.6 L823.3,540.9 L823.1,546.9 L822.9,552.6 L822.7,558.0 L822.5,563.2 L822.3,568.2 L822.1,573.1 L821.8,577.8 L821.6,582.3 L821.3,586.7 L821.0,591.0 L820.7,595.2 L820.4,599.2 L820.0,603.2 L819.7,607.0 L819.3,610.8 L818.9,614.5 L818.5,618.1 L818.1,621.6 L817.7,625.1 L817.3,628.5 L816.8,631.8 L816.3,635.0 L815.8,638.2 L815.3,641.4 L814.8,644.5 L814.3,647.5 L813.7,650.5 L813.2,653.4 L812.6,656.3 L812.0,659.2 L811.4,662.0 L810.7,664.7 L810.1,667.5 L809.4,670.1 L808.8,672.8 L808.1,675.4 L807.4,677.9 L806.6,680.4 L805.9,682.9 L805.1,685.4 L804.4,687.8 L803.6,690.2 L802.8,692.5 L802.0,694.9 L801.1,697.2 L800.3,699.4 L799.4,701.7 L798.5,703.9 L797.6,706.0 L796.7,708.2 L795.8,710.3 L794.8,712.4 L793.9,714.4 L792.9,716.5 L791.9,718.5 L790.9,720.5 L789.9,722.4 L788.8,724.4 L787.7,726.3 L786.7,728.2 L785.6,730.0 L784.4,731.9 L783.3,733.7 L782.2,735.5 L781.0,737.3 L779.8,739.0 L778.6,740.8 L777.4,742.5 L776.2,744.1 L774.9,745.8 L773.6,747.5 L772.3,749.1 L771.0,750.7 L769.7,752.3 L768.3,753.8 L767.0,755.4 L765.6,756.9 L764.2,758.4 L762.8,759.9 L761.3,761.3 L759.9,762.8 L758.4,764.2 L756.9,765.6 L755.4,767.0 L753.8,768.3 L752.3,769.7 L750.7,771.0 L749.1,772.3 L747.5,773.6 L745.8,774.9 L744.1,776.2 L742.5,777.4 L740.8,778.6 L739.0,779.8 L737.3,781.0 L735.5,782.2 L733.7,783.3 L731.9,784.4 L730.0,785.6 L728.2,786.7 L726.3,787.7 L724.4,788.8 L722.4,789.9 L720.5,790.9 L718.5,791.9 L716.5,792.9 L714.4,793.9 L712.4,794.8 L710.3,795.8 L708.2,796.7 L706.0,797.6 L703.9,798.5 L701.7,799.4 L699.4,800.3 L697.2,801.1 L694.9,802.0 L692.5,802.8 L690.2,803.6 L687.8,804.4 L685.4,805.1 L682.9,805.9 L680.4,806.6 L677.9,807.4 L675.4,808.1 L672.8,808.8 L670.1,809.4 L667.5,810.1 L664.7,810.7 L662.0,811.4 L659.2,812.0 L656.3,812.6 L653.4,813.2 L650.5,813.7 L647.5,814.3 L644.5,814.8 L641.4,815.3 L638.2,815.8 L635.0,816.3 L631.8,816.8 L628.5,817.3 L625.1,817.7 L621.6,818.1 L618.1,818.5 L614.5,818.9 L610.8,819.3 L607.0,819.7 L603.2,820.0 L599.2,820.4 L595.2,820.7 L591.0,821.0 L586.7,821.3 L582.3,821.6 L577.8,821.8 L573.1,822.1 L568.2,822.3 L563.2,822.5 L558.0,822.7 L552.6,822.9 L546.9,823.1 L540.9,823.3 L534.6,823.4 L527.9,823.5 L520.8,823.6 L513.1,823.7 L504.7,823.8 L495.4,823.9 L484.7,823.9 L471.9,824.0 L455.1,824.0 L412.0,824.0 L368.9,824.0 L352.1,824.0 L339.3,823.9 L328.6,823.9 L319.3,823.8 L310.9,823.7 L303.2,823.6 L296.1,823.5 L289.4,823.4 L283.1,823.3 L277.1,823.1 L271.4,822.9 L266.0,822.7 L260.8,822.5 L255.8,822.3 L250.9,822.1 L246.2,821.8 L241.7,821.6 L237.3,821.3 L233.0,821.0 L228.8,820.7 L224.8,820.4 L220.8,820.0 L217.0,819.7 L213.2,819.3 L209.5,818.9 L205.9,818.5 L202.4,818.1 L198.9,817.7 L195.5,817.3 L192.2,816.8 L189.0,816.3 L185.8,815.8 L182.6,815.3 L179.5,814.8 L176.5,814.3 L173.5,813.7 L170.6,813.2 L167.7,812.6 L164.8,812.0 L162.0,811.4 L159.3,810.7 L156.5,810.1 L153.9,809.4 L151.2,808.8 L148.6,808.1 L146.1,807.4 L143.6,806.6 L141.1,805.9 L138.6,805.1 L136.2,804.4 L133.8,803.6 L131.5,802.8 L129.1,802.0 L126.8,801.1 L124.6,800.3 L122.3,799.4 L120.1,798.5 L118.0,797.6 L115.8,796.7 L113.7,795.8 L111.6,794.8 L109.6,793.9 L107.5,792.9 L105.5,791.9 L103.5,790.9 L101.6,789.9 L99.6,788.8 L97.7,787.7 L95.8,786.7 L94.0,785.6 L92.1,784.4 L90.3,783.3 L88.5,782.2 L86.7,781.0 L85.0,779.8 L83.2,778.6 L81.5,777.4 L79.9,776.2 L78.2,774.9 L76.5,773.6 L74.9,772.3 L73.3,771.0 L71.7,769.7 L70.2,768.3 L68.6,767.0 L67.1,765.6 L65.6,764.2 L64.1,762.8 L62.7,761.3 L61.2,759.9 L59.8,758.4 L58.4,756.9 L57.0,755.4 L55.7,753.8 L54.3,752.3 L53.0,750.7 L51.7,749.1 L50.4,747.5 L49.1,745.8 L47.8,744.1 L46.6,742.5 L45.4,740.8 L44.2,739.0 L43.0,737.3 L41.8,735.5 L40.7,733.7 L39.6,731.9 L38.4,730.0 L37.3,728.2 L36.3,726.3 L35.2,724.4 L34.1,722.4 L33.1,720.5 L32.1,718.5 L31.1,716.5 L30.1,714.4 L29.2,712.4 L28.2,710.3 L27.3,708.2 L26.4,706.0 L25.5,703.9 L24.6,701.7 L23.7,699.4 L22.9,697.2 L22.0,694.9 L21.2,692.5 L20.4,690.2 L19.6,687.8 L18.9,685.4 L18.1,682.9 L17.4,680.4 L16.6,677.9 L15.9,675.4 L15.2,672.8 L14.6,670.1 L13.9,667.5 L13.3,664.7 L12.6,662.0 L12.0,659.2 L11.4,656.3 L10.8,653.4 L10.3,650.5 L9.7,647.5 L9.2,644.5 L8.7,641.4 L8.2,638.2 L7.7,635.0 L7.2,631.8 L6.7,628.5 L6.3,625.1 L5.9,621.6 L5.5,618.1 L5.1,614.5 L4.7,610.8 L4.3,607.0 L4.0,603.2 L3.6,599.2 L3.3,595.2 L3.0,591.0 L2.7,586.7 L2.4,582.3 L2.2,577.8 L1.9,573.1 L1.7,568.2 L1.5,563.2 L1.3,558.0 L1.1,552.6 L0.9,546.9 L0.7,540.9 L0.6,534.6 L0.5,527.9 L0.4,520.8 L0.3,513.1 L0.2,504.7 L0.1,495.4 L0.1,484.7 L0.0,471.9 L0.0,455.1 L0.0,412.0 L0.0,368.9 L0.0,352.1 L0.1,339.3 L0.1,328.6 L0.2,319.3 L0.3,310.9 L0.4,303.2 L0.5,296.1 L0.6,289.4 L0.7,283.1 L0.9,277.1 L1.1,271.4 L1.3,266.0 L1.5,260.8 L1.7,255.8 L1.9,250.9 L2.2,246.2 L2.4,241.7 L2.7,237.3 L3.0,233.0 L3.3,228.8 L3.6,224.8 L4.0,220.8 L4.3,217.0 L4.7,213.2 L5.1,209.5 L5.5,205.9 L5.9,202.4 L6.3,198.9 L6.7,195.5 L7.2,192.2 L7.7,189.0 L8.2,185.8 L8.7,182.6 L9.2,179.5 L9.7,176.5 L10.3,173.5 L10.8,170.6 L11.4,167.7 L12.0,164.8 L12.6,162.0 L13.3,159.3 L13.9,156.5 L14.6,153.9 L15.2,151.2 L15.9,148.6 L16.6,146.1 L17.4,143.6 L18.1,141.1 L18.9,138.6 L19.6,136.2 L20.4,133.8 L21.2,131.5 L22.0,129.1 L22.9,126.8 L23.7,124.6 L24.6,122.3 L25.5,120.1 L26.4,118.0 L27.3,115.8 L28.2,113.7 L29.2,111.6 L30.1,109.6 L31.1,107.5 L32.1,105.5 L33.1,103.5 L34.1,101.6 L35.2,99.6 L36.3,97.7 L37.3,95.8 L38.4,94.0 L39.6,92.1 L40.7,90.3 L41.8,88.5 L43.0,86.7 L44.2,85.0 L45.4,83.2 L46.6,81.5 L47.8,79.9 L49.1,78.2 L50.4,76.5 L51.7,74.9 L53.0,73.3 L54.3,71.7 L55.7,70.2 L57.0,68.6 L58.4,67.1 L59.8,65.6 L61.2,64.1 L62.7,62.7 L64.1,61.2 L65.6,59.8 L67.1,58.4 L68.6,57.0 L70.2,55.7 L71.7,54.3 L73.3,53.0 L74.9,51.7 L76.5,50.4 L78.2,49.1 L79.9,47.8 L81.5,46.6 L83.2,45.4 L85.0,44.2 L86.7,43.0 L88.5,41.8 L90.3,40.7 L92.1,39.6 L94.0,38.4 L95.8,37.3 L97.7,36.3 L99.6,35.2 L101.6,34.1 L103.5,33.1 L105.5,32.1 L107.5,31.1 L109.6,30.1 L111.6,29.2 L113.7,28.2 L115.8,27.3 L118.0,26.4 L120.1,25.5 L122.3,24.6 L124.6,23.7 L126.8,22.9 L129.1,22.0 L131.5,21.2 L133.8,20.4 L136.2,19.6 L138.6,18.9 L141.1,18.1 L143.6,17.4 L146.1,16.6 L148.6,15.9 L151.2,15.2 L153.9,14.6 L156.5,13.9 L159.3,13.3 L162.0,12.6 L164.8,12.0 L167.7,11.4 L170.6,10.8 L173.5,10.3 L176.5,9.7 L179.5,9.2 L182.6,8.7 L185.8,8.2 L189.0,7.7 L192.2,7.2 L195.5,6.7 L198.9,6.3 L202.4,5.9 L205.9,5.5 L209.5,5.1 L213.2,4.7 L217.0,4.3 L220.8,4.0 L224.8,3.6 L228.8,3.3 L233.0,3.0 L237.3,2.7 L241.7,2.4 L246.2,2.2 L250.9,1.9 L255.8,1.7 L260.8,1.5 L266.0,1.3 L271.4,1.1 L277.1,0.9 L283.1,0.7 L289.4,0.6 L296.1,0.5 L303.2,0.4 L310.9,0.3 L319.3,0.2 L328.6,0.1 L339.3,0.1 L352.1,0.0 L368.9,0.0 L412.0,0.0 L455.1,0.0 L471.9,0.0 L484.7,0.1 L495.4,0.1 L504.7,0.2 L513.1,0.3 L520.8,0.4 L527.9,0.5 L534.6,0.6 L540.9,0.7 L546.9,0.9 L552.6,1.1 L558.0,1.3 L563.2,1.5 L568.2,1.7 L573.1,1.9 L577.8,2.2 L582.3,2.4 L586.7,2.7 L591.0,3.0 L595.2,3.3 L599.2,3.6 L603.2,4.0 L607.0,4.3 L610.8,4.7 L614.5,5.1 L618.1,5.5 L621.6,5.9 L625.1,6.3 L628.5,6.7 L631.8,7.2 L635.0,7.7 L638.2,8.2 L641.4,8.7 L644.5,9.2 L647.5,9.7 L650.5,10.3 L653.4,10.8 L656.3,11.4 L659.2,12.0 L662.0,12.6 L664.7,13.3 L667.5,13.9 L670.1,14.6 L672.8,15.2 L675.4,15.9 L677.9,16.6 L680.4,17.4 L682.9,18.1 L685.4,18.9 L687.8,19.6 L690.2,20.4 L692.5,21.2 L694.9,22.0 L697.2,22.9 L699.4,23.7 L701.7,24.6 L703.9,25.5 L706.0,26.4 L708.2,27.3 L710.3,28.2 L712.4,29.2 L714.4,30.1 L716.5,31.1 L718.5,32.1 L720.5,33.1 L722.4,34.1 L724.4,35.2 L726.3,36.3 L728.2,37.3 L730.0,38.4 L731.9,39.6 L733.7,40.7 L735.5,41.8 L737.3,43.0 L739.0,44.2 L740.8,45.4 L742.5,46.6 L744.1,47.8 L745.8,49.1 L747.5,50.4 L749.1,51.7 L750.7,53.0 L752.3,54.3 L753.8,55.7 L755.4,57.0 L756.9,58.4 L758.4,59.8 L759.9,61.2 L761.3,62.7 L762.8,64.1 L764.2,65.6 L765.6,67.1 L767.0,68.6 L768.3,70.2 L769.7,71.7 L771.0,73.3 L772.3,74.9 L773.6,76.5 L774.9,78.2 L776.2,79.9 L777.4,81.5 L778.6,83.2 L779.8,85.0 L781.0,86.7 L782.2,88.5 L783.3,90.3 L784.4,92.1 L785.6,94.0 L786.7,95.8 L787.7,97.7 L788.8,99.6 L789.9,101.6 L790.9,103.5 L791.9,105.5 L792.9,107.5 L793.9,109.6 L794.8,111.6 L795.8,113.7 L796.7,115.8 L797.6,118.0 L798.5,120.1 L799.4,122.3 L800.3,124.6 L801.1,126.8 L802.0,129.1 L802.8,131.5 L803.6,133.8 L804.4,136.2 L805.1,138.6 L805.9,141.1 L806.6,143.6 L807.4,146.1 L808.1,148.6 L808.8,151.2 L809.4,153.9 L810.1,156.5 L810.7,159.3 L811.4,162.0 L812.0,164.8 L812.6,167.7 L813.2,170.6 L813.7,173.5 L814.3,176.5 L814.8,179.5 L815.3,182.6 L815.8,185.8 L816.3,189.0 L816.8,192.2 L817.3,195.5 L817.7,198.9 L818.1,202.4 L818.5,205.9 L818.9,209.5 L819.3,213.2 L819.7,217.0 L820.0,220.8 L820.4,224.8 L820.7,228.8 L821.0,233.0 L821.3,237.3 L821.6,241.7 L821.8,246.2 L822.1,250.9 L822.3,255.8 L822.5,260.8 L822.7,266.0 L822.9,271.4 L823.1,277.1 L823.3,283.1 L823.4,289.4 L823.5,296.1 L823.6,303.2 L823.7,310.9 L823.8,319.3 L823.9,328.6 L823.9,339.3 L824.0,352.1 L824.0,368.9 Z" transform="translate(100 112)" fill="#000" opacity="0.35" filter="url(#onicode-tile-shadow)"/>
<g transform="translate(100 100)">
<path d="M824.0,412.0 L824.0,455.1 L824.0,471.9 L823.9,484.7 L823.9,495.4 L823.8,504.7 L823.7,513.1 L823.6,520.8 L823.5,527.9 L823.4,534.6 L823.3,540.9 L823.1,546.9 L822.9,552.6 L822.7,558.0 L822.5,563.2 L822.3,568.2 L822.1,573.1 L821.8,577.8 L821.6,582.3 L821.3,586.7 L821.0,591.0 L820.7,595.2 L820.4,599.2 L820.0,603.2 L819.7,607.0 L819.3,610.8 L818.9,614.5 L818.5,618.1 L818.1,621.6 L817.7,625.1 L817.3,628.5 L816.8,631.8 L816.3,635.0 L815.8,638.2 L815.3,641.4 L814.8,644.5 L814.3,647.5 L813.7,650.5 L813.2,653.4 L812.6,656.3 L812.0,659.2 L811.4,662.0 L810.7,664.7 L810.1,667.5 L809.4,670.1 L808.8,672.8 L808.1,675.4 L807.4,677.9 L806.6,680.4 L805.9,682.9 L805.1,685.4 L804.4,687.8 L803.6,690.2 L802.8,692.5 L802.0,694.9 L801.1,697.2 L800.3,699.4 L799.4,701.7 L798.5,703.9 L797.6,706.0 L796.7,708.2 L795.8,710.3 L794.8,712.4 L793.9,714.4 L792.9,716.5 L791.9,718.5 L790.9,720.5 L789.9,722.4 L788.8,724.4 L787.7,726.3 L786.7,728.2 L785.6,730.0 L784.4,731.9 L783.3,733.7 L782.2,735.5 L781.0,737.3 L779.8,739.0 L778.6,740.8 L777.4,742.5 L776.2,744.1 L774.9,745.8 L773.6,747.5 L772.3,749.1 L771.0,750.7 L769.7,752.3 L768.3,753.8 L767.0,755.4 L765.6,756.9 L764.2,758.4 L762.8,759.9 L761.3,761.3 L759.9,762.8 L758.4,764.2 L756.9,765.6 L755.4,767.0 L753.8,768.3 L752.3,769.7 L750.7,771.0 L749.1,772.3 L747.5,773.6 L745.8,774.9 L744.1,776.2 L742.5,777.4 L740.8,778.6 L739.0,779.8 L737.3,781.0 L735.5,782.2 L733.7,783.3 L731.9,784.4 L730.0,785.6 L728.2,786.7 L726.3,787.7 L724.4,788.8 L722.4,789.9 L720.5,790.9 L718.5,791.9 L716.5,792.9 L714.4,793.9 L712.4,794.8 L710.3,795.8 L708.2,796.7 L706.0,797.6 L703.9,798.5 L701.7,799.4 L699.4,800.3 L697.2,801.1 L694.9,802.0 L692.5,802.8 L690.2,803.6 L687.8,804.4 L685.4,805.1 L682.9,805.9 L680.4,806.6 L677.9,807.4 L675.4,808.1 L672.8,808.8 L670.1,809.4 L667.5,810.1 L664.7,810.7 L662.0,811.4 L659.2,812.0 L656.3,812.6 L653.4,813.2 L650.5,813.7 L647.5,814.3 L644.5,814.8 L641.4,815.3 L638.2,815.8 L635.0,816.3 L631.8,816.8 L628.5,817.3 L625.1,817.7 L621.6,818.1 L618.1,818.5 L614.5,818.9 L610.8,819.3 L607.0,819.7 L603.2,820.0 L599.2,820.4 L595.2,820.7 L591.0,821.0 L586.7,821.3 L582.3,821.6 L577.8,821.8 L573.1,822.1 L568.2,822.3 L563.2,822.5 L558.0,822.7 L552.6,822.9 L546.9,823.1 L540.9,823.3 L534.6,823.4 L527.9,823.5 L520.8,823.6 L513.1,823.7 L504.7,823.8 L495.4,823.9 L484.7,823.9 L471.9,824.0 L455.1,824.0 L412.0,824.0 L368.9,824.0 L352.1,824.0 L339.3,823.9 L328.6,823.9 L319.3,823.8 L310.9,823.7 L303.2,823.6 L296.1,823.5 L289.4,823.4 L283.1,823.3 L277.1,823.1 L271.4,822.9 L266.0,822.7 L260.8,822.5 L255.8,822.3 L250.9,822.1 L246.2,821.8 L241.7,821.6 L237.3,821.3 L233.0,821.0 L228.8,820.7 L224.8,820.4 L220.8,820.0 L217.0,819.7 L213.2,819.3 L209.5,818.9 L205.9,818.5 L202.4,818.1 L198.9,817.7 L195.5,817.3 L192.2,816.8 L189.0,816.3 L185.8,815.8 L182.6,815.3 L179.5,814.8 L176.5,814.3 L173.5,813.7 L170.6,813.2 L167.7,812.6 L164.8,812.0 L162.0,811.4 L159.3,810.7 L156.5,810.1 L153.9,809.4 L151.2,808.8 L148.6,808.1 L146.1,807.4 L143.6,806.6 L141.1,805.9 L138.6,805.1 L136.2,804.4 L133.8,803.6 L131.5,802.8 L129.1,802.0 L126.8,801.1 L124.6,800.3 L122.3,799.4 L120.1,798.5 L118.0,797.6 L115.8,796.7 L113.7,795.8 L111.6,794.8 L109.6,793.9 L107.5,792.9 L105.5,791.9 L103.5,790.9 L101.6,789.9 L99.6,788.8 L97.7,787.7 L95.8,786.7 L94.0,785.6 L92.1,784.4 L90.3,783.3 L88.5,782.2 L86.7,781.0 L85.0,779.8 L83.2,778.6 L81.5,777.4 L79.9,776.2 L78.2,774.9 L76.5,773.6 L74.9,772.3 L73.3,771.0 L71.7,769.7 L70.2,768.3 L68.6,767.0 L67.1,765.6 L65.6,764.2 L64.1,762.8 L62.7,761.3 L61.2,759.9 L59.8,758.4 L58.4,756.9 L57.0,755.4 L55.7,753.8 L54.3,752.3 L53.0,750.7 L51.7,749.1 L50.4,747.5 L49.1,745.8 L47.8,744.1 L46.6,742.5 L45.4,740.8 L44.2,739.0 L43.0,737.3 L41.8,735.5 L40.7,733.7 L39.6,731.9 L38.4,730.0 L37.3,728.2 L36.3,726.3 L35.2,724.4 L34.1,722.4 L33.1,720.5 L32.1,718.5 L31.1,716.5 L30.1,714.4 L29.2,712.4 L28.2,710.3 L27.3,708.2 L26.4,706.0 L25.5,703.9 L24.6,701.7 L23.7,699.4 L22.9,697.2 L22.0,694.9 L21.2,692.5 L20.4,690.2 L19.6,687.8 L18.9,685.4 L18.1,682.9 L17.4,680.4 L16.6,677.9 L15.9,675.4 L15.2,672.8 L14.6,670.1 L13.9,667.5 L13.3,664.7 L12.6,662.0 L12.0,659.2 L11.4,656.3 L10.8,653.4 L10.3,650.5 L9.7,647.5 L9.2,644.5 L8.7,641.4 L8.2,638.2 L7.7,635.0 L7.2,631.8 L6.7,628.5 L6.3,625.1 L5.9,621.6 L5.5,618.1 L5.1,614.5 L4.7,610.8 L4.3,607.0 L4.0,603.2 L3.6,599.2 L3.3,595.2 L3.0,591.0 L2.7,586.7 L2.4,582.3 L2.2,577.8 L1.9,573.1 L1.7,568.2 L1.5,563.2 L1.3,558.0 L1.1,552.6 L0.9,546.9 L0.7,540.9 L0.6,534.6 L0.5,527.9 L0.4,520.8 L0.3,513.1 L0.2,504.7 L0.1,495.4 L0.1,484.7 L0.0,471.9 L0.0,455.1 L0.0,412.0 L0.0,368.9 L0.0,352.1 L0.1,339.3 L0.1,328.6 L0.2,319.3 L0.3,310.9 L0.4,303.2 L0.5,296.1 L0.6,289.4 L0.7,283.1 L0.9,277.1 L1.1,271.4 L1.3,266.0 L1.5,260.8 L1.7,255.8 L1.9,250.9 L2.2,246.2 L2.4,241.7 L2.7,237.3 L3.0,233.0 L3.3,228.8 L3.6,224.8 L4.0,220.8 L4.3,217.0 L4.7,213.2 L5.1,209.5 L5.5,205.9 L5.9,202.4 L6.3,198.9 L6.7,195.5 L7.2,192.2 L7.7,189.0 L8.2,185.8 L8.7,182.6 L9.2,179.5 L9.7,176.5 L10.3,173.5 L10.8,170.6 L11.4,167.7 L12.0,164.8 L12.6,162.0 L13.3,159.3 L13.9,156.5 L14.6,153.9 L15.2,151.2 L15.9,148.6 L16.6,146.1 L17.4,143.6 L18.1,141.1 L18.9,138.6 L19.6,136.2 L20.4,133.8 L21.2,131.5 L22.0,129.1 L22.9,126.8 L23.7,124.6 L24.6,122.3 L25.5,120.1 L26.4,118.0 L27.3,115.8 L28.2,113.7 L29.2,111.6 L30.1,109.6 L31.1,107.5 L32.1,105.5 L33.1,103.5 L34.1,101.6 L35.2,99.6 L36.3,97.7 L37.3,95.8 L38.4,94.0 L39.6,92.1 L40.7,90.3 L41.8,88.5 L43.0,86.7 L44.2,85.0 L45.4,83.2 L46.6,81.5 L47.8,79.9 L49.1,78.2 L50.4,76.5 L51.7,74.9 L53.0,73.3 L54.3,71.7 L55.7,70.2 L57.0,68.6 L58.4,67.1 L59.8,65.6 L61.2,64.1 L62.7,62.7 L64.1,61.2 L65.6,59.8 L67.1,58.4 L68.6,57.0 L70.2,55.7 L71.7,54.3 L73.3,53.0 L74.9,51.7 L76.5,50.4 L78.2,49.1 L79.9,47.8 L81.5,46.6 L83.2,45.4 L85.0,44.2 L86.7,43.0 L88.5,41.8 L90.3,40.7 L92.1,39.6 L94.0,38.4 L95.8,37.3 L97.7,36.3 L99.6,35.2 L101.6,34.1 L103.5,33.1 L105.5,32.1 L107.5,31.1 L109.6,30.1 L111.6,29.2 L113.7,28.2 L115.8,27.3 L118.0,26.4 L120.1,25.5 L122.3,24.6 L124.6,23.7 L126.8,22.9 L129.1,22.0 L131.5,21.2 L133.8,20.4 L136.2,19.6 L138.6,18.9 L141.1,18.1 L143.6,17.4 L146.1,16.6 L148.6,15.9 L151.2,15.2 L153.9,14.6 L156.5,13.9 L159.3,13.3 L162.0,12.6 L164.8,12.0 L167.7,11.4 L170.6,10.8 L173.5,10.3 L176.5,9.7 L179.5,9.2 L182.6,8.7 L185.8,8.2 L189.0,7.7 L192.2,7.2 L195.5,6.7 L198.9,6.3 L202.4,5.9 L205.9,5.5 L209.5,5.1 L213.2,4.7 L217.0,4.3 L220.8,4.0 L224.8,3.6 L228.8,3.3 L233.0,3.0 L237.3,2.7 L241.7,2.4 L246.2,2.2 L250.9,1.9 L255.8,1.7 L260.8,1.5 L266.0,1.3 L271.4,1.1 L277.1,0.9 L283.1,0.7 L289.4,0.6 L296.1,0.5 L303.2,0.4 L310.9,0.3 L319.3,0.2 L328.6,0.1 L339.3,0.1 L352.1,0.0 L368.9,0.0 L412.0,0.0 L455.1,0.0 L471.9,0.0 L484.7,0.1 L495.4,0.1 L504.7,0.2 L513.1,0.3 L520.8,0.4 L527.9,0.5 L534.6,0.6 L540.9,0.7 L546.9,0.9 L552.6,1.1 L558.0,1.3 L563.2,1.5 L568.2,1.7 L573.1,1.9 L577.8,2.2 L582.3,2.4 L586.7,2.7 L591.0,3.0 L595.2,3.3 L599.2,3.6 L603.2,4.0 L607.0,4.3 L610.8,4.7 L614.5,5.1 L618.1,5.5 L621.6,5.9 L625.1,6.3 L628.5,6.7 L631.8,7.2 L635.0,7.7 L638.2,8.2 L641.4,8.7 L644.5,9.2 L647.5,9.7 L650.5,10.3 L653.4,10.8 L656.3,11.4 L659.2,12.0 L662.0,12.6 L664.7,13.3 L667.5,13.9 L670.1,14.6 L672.8,15.2 L675.4,15.9 L677.9,16.6 L680.4,17.4 L682.9,18.1 L685.4,18.9 L687.8,19.6 L690.2,20.4 L692.5,21.2 L694.9,22.0 L697.2,22.9 L699.4,23.7 L701.7,24.6 L703.9,25.5 L706.0,26.4 L708.2,27.3 L710.3,28.2 L712.4,29.2 L714.4,30.1 L716.5,31.1 L718.5,32.1 L720.5,33.1 L722.4,34.1 L724.4,35.2 L726.3,36.3 L728.2,37.3 L730.0,38.4 L731.9,39.6 L733.7,40.7 L735.5,41.8 L737.3,43.0 L739.0,44.2 L740.8,45.4 L742.5,46.6 L744.1,47.8 L745.8,49.1 L747.5,50.4 L749.1,51.7 L750.7,53.0 L752.3,54.3 L753.8,55.7 L755.4,57.0 L756.9,58.4 L758.4,59.8 L759.9,61.2 L761.3,62.7 L762.8,64.1 L764.2,65.6 L765.6,67.1 L767.0,68.6 L768.3,70.2 L769.7,71.7 L771.0,73.3 L772.3,74.9 L773.6,76.5 L774.9,78.2 L776.2,79.9 L777.4,81.5 L778.6,83.2 L779.8,85.0 L781.0,86.7 L782.2,88.5 L783.3,90.3 L784.4,92.1 L785.6,94.0 L786.7,95.8 L787.7,97.7 L788.8,99.6 L789.9,101.6 L790.9,103.5 L791.9,105.5 L792.9,107.5 L793.9,109.6 L794.8,111.6 L795.8,113.7 L796.7,115.8 L797.6,118.0 L798.5,120.1 L799.4,122.3 L800.3,124.6 L801.1,126.8 L802.0,129.1 L802.8,131.5 L803.6,133.8 L804.4,136.2 L805.1,138.6 L805.9,141.1 L806.6,143.6 L807.4,146.1 L808.1,148.6 L808.8,151.2 L809.4,153.9 L810.1,156.5 L810.7,159.3 L811.4,162.0 L812.0,164.8 L812.6,167.7 L813.2,170.6 L813.7,173.5 L814.3,176.5 L814.8,179.5 L815.3,182.6 L815.8,185.8 L816.3,189.0 L816.8,192.2 L817.3,195.5 L817.7,198.9 L818.1,202.4 L818.5,205.9 L818.9,209.5 L819.3,213.2 L819.7,217.0 L820.0,220.8 L820.4,224.8 L820.7,228.8 L821.0,233.0 L821.3,237.3 L821.6,241.7 L821.8,246.2 L822.1,250.9 L822.3,255.8 L822.5,260.8 L822.7,266.0 L822.9,271.4 L823.1,277.1 L823.3,283.1 L823.4,289.4 L823.5,296.1 L823.6,303.2 L823.7,310.9 L823.8,319.3 L823.9,328.6 L823.9,339.3 L824.0,352.1 L824.0,368.9 Z" fill="#FF2D55"/>
<g transform="translate(57.7 63.7) scale(0.86)">
<g transform="translate(10 22)" opacity="0.28" filter="url(#onicode-char-shadow)"><g transform="scale(0.5) translate(0.000000,1648.000000) scale(0.100000,-0.100000)"><path fill="#5A0016" fill-rule="evenodd" d="M6699 14264 c-465 -84 -855 -511 -1123 -1226 -20 -53 -48 -139 -62 -190 -44 -162 -22 -142 -270 -243 -1485 -605 -2685 -1711 -3375 -3108 -488 -987 -590 -1968 -303 -2896 158 -511 479 -1089 929 -1676 257 -335 725 -806 1058 -1063 45 -35 120 -94 165 -130 233 -186 688 -478 1027 -659 874 -466 1906 -792 2990 -944 669 -94 1529 -105 2210 -28 2311 258 3992 1487 4548 3324 373 1234 166 2791 -550 4120 -313 583 -624 996 -1176 1565 -99 101 -183 194 -187 206 -5 12 -4 51 1 86 5 34 12 153 15 263 26 874 -325 1537 -890 1680 -243 62 -470 34 -673 -81 -170 -97 -264 -194 -416 -433 -51 -79 -115 -168 -144 -199 -63 -67 -60 -67 -274 16 -679 262 -1388 409 -2114 438 -101 4 -157 11 -172 20 -28 17 -64 132 -92 294 -106 600 -577 963 -1122 864z"/></g></g>
<g transform="scale(0.5) translate(0.000000,1648.000000) scale(0.100000,-0.100000)"><path fill="#0B0B0C" fill-rule="evenodd" d="M6699 14264 c-465 -84 -855 -511 -1123 -1226 -20 -53 -48 -139 -62 -190 -44 -162 -22 -142 -270 -243 -1485 -605 -2685 -1711 -3375 -3108 -488 -987 -590 -1968 -303 -2896 158 -511 479 -1089 929 -1676 257 -335 725 -806 1058 -1063 45 -35 120 -94 165 -130 233 -186 688 -478 1027 -659 874 -466 1906 -792 2990 -944 669 -94 1529 -105 2210 -28 2311 258 3992 1487 4548 3324 373 1234 166 2791 -550 4120 -313 583 -624 996 -1176 1565 -99 101 -183 194 -187 206 -5 12 -4 51 1 86 5 34 12 153 15 263 26 874 -325 1537 -890 1680 -243 62 -470 34 -673 -81 -170 -97 -264 -194 -416 -433 -51 -79 -115 -168 -144 -199 -63 -67 -60 -67 -274 16 -679 262 -1388 409 -2114 438 -101 4 -157 11 -172 20 -28 17 -64 132 -92 294 -106 600 -577 963 -1122 864z"/></g><g transform="scale(0.5) translate(0.000000,1648.000000) scale(0.100000,-0.100000)"><path fill="url(#onicode-body)" fill-rule="evenodd" d="M6701 13545 c-210 -101 -428 -450 -531 -849 -32 -126 -45 -211 -64 -414 -16 -171 -22 -181 -130 -213 -637 -186 -1114 -404 -1619 -741 -996 -665 -1715 -1611 -2002 -2633 -342 -1217 130 -2372 1375 -3366 746 -595 1885 -1110 3005 -1358 1658 -367 3243 -254 4370 314 746 375 1289 912 1644 1623 356 714 473 1526 335 2342 -152 903 -586 1741 -1268 2448 -163 169 -168 179 -136 268 123 334 164 544 164 839 1 500 -161 865 -384 865 -118 -1 -178 -51 -318 -265 -173 -264 -321 -423 -509 -549 -204 -135 -250 -144 -403 -76 -765 339 -1710 550 -2463 550 -205 0 -226 7 -307 103 -172 202 -284 456 -351 795 -33 166 -64 245 -116 294 -72 69 -178 77 -292 23z"/></g><path d="M213.8,348.2 Q262.2,322.8 310.8,373.2" fill="none" stroke="#0B0B0C" stroke-width="37.5" stroke-linecap="round"/><path d="M433.0,413.8 Q483.8,382.5 534.5,413.2" fill="none" stroke="#0B0B0C" stroke-width="39.0" stroke-linecap="round"/>
</g>
</g>
</svg>`,
  react: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="15" fill="#20232a"/>
  <g fill="none" stroke="#61dafb" stroke-width="2.8"><ellipse cx="32" cy="32" rx="25" ry="9"/><ellipse cx="32" cy="32" rx="25" ry="9" transform="rotate(60 32 32)"/><ellipse cx="32" cy="32" rx="25" ry="9" transform="rotate(120 32 32)"/></g>
  <circle cx="32" cy="32" r="4.8" fill="#61dafb"/>
</svg>`,
  linux: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="15" fill="#f7c948"/>
  <ellipse cx="32" cy="35" rx="17" ry="22" fill="#202124"/>
  <ellipse cx="32" cy="40" rx="12" ry="14" fill="#f5f5f2"/>
  <circle cx="27" cy="24" r="5" fill="white"/><circle cx="37" cy="24" r="5" fill="white"/>
  <circle cx="28" cy="25" r="2"/><circle cx="36" cy="25" r="2"/>
  <path d="M27 31l5-4 5 4-5 4z" fill="#f28c28"/><path d="M16 55h14l-7-5zM34 55h14l-7-5z" fill="#f28c28"/>
</svg>`,
} as const;

export const SHOWCASE_PROJECTS = [
  {
    id: "onicode",
    title: "Onicode",
    directory: "onicode",
    repositoryUrl: "https://github.com/aislopware/onicode.git",
    favicon: PROJECT_FAVICONS.onicode,
  },
  {
    id: "react",
    title: "React",
    directory: "react",
    repositoryUrl: "https://github.com/facebook/react.git",
    favicon: PROJECT_FAVICONS.react,
  },
  {
    id: "linux",
    title: "Linux",
    directory: "linux",
    repositoryUrl: "https://github.com/torvalds/linux.git",
    favicon: PROJECT_FAVICONS.linux,
  },
] as const;

export const SHOWCASE_ENVIRONMENTS = [
  {
    id: "moonbase-terminal",
    label: "Moonbase Terminal",
    projectIds: ["onicode"],
  },
  {
    id: "suspense-station",
    label: "Suspense Station",
    projectIds: ["react"],
  },
  {
    id: "kernel-cabin",
    label: "Kernel Cabin",
    projectIds: ["linux"],
  },
] as const;

export const SHOWCASE_THREADS = [
  {
    id: SHOWCASE_THREAD_ID,
    projectId: "onicode",
    title: "Make remote coding feel local ✦",
    branch: "feat/remote-command-center",
    minutesAgo: 3,
    request:
      "Give Onicode a remote-first command center. Make three machines feel one tap away, keep agent work in sync, and make every handoff feel instant.",
    response:
      "Onicode now treats every machine like it is right here in the room. ✦\n\n- Moonbase, Suspense Station, and Kernel Cabin stay live together\n- Terminal state follows you without losing a single line\n- Agent work remains perfectly in sync across devices\n- Handoffs land before your train of thought can wander\n\nI also ran the changed workspace: **612 tests passed**.",
  },
  {
    id: "pocket-command-center",
    projectId: "onicode",
    title: "Put the command center in your pocket",
    branch: "feat/pocket-command-center",
    minutesAgo: 21,
    state: "approval" as const,
    request: "Make switching between desktop, phone, and tablet feel like one continuous session.",
    response:
      "The handoff flow preserves the selected thread, terminal buffer, and working diff. The final motion treatment is ready for approval.",
  },
  {
    id: "buttery-suspense",
    projectId: "react",
    title: "Make Suspense transitions buttery",
    branch: "perf/buttery-suspense",
    minutesAgo: 12,
    state: "working" as const,
    request:
      "Trace the last few dropped frames in nested Suspense transitions and make them disappear.",
    response: null,
  },
  {
    id: "hydration-haikus",
    projectId: "react",
    title: "Turn hydration warnings into haikus",
    branch: "dev/hydration-haikus",
    minutesAgo: 44,
    request:
      "Keep hydration errors precise, but make the development copy unexpectedly delightful.",
    response:
      "The diagnostics still lead with the exact mismatch and component stack. A tiny optional haiku now closes the expanded explanation.",
    snoozeMinutes: 90,
  },
  {
    id: "beautiful-boot",
    projectId: "linux",
    title: "Make boot logs oddly beautiful",
    branch: "feat/beautiful-boot",
    minutesAgo: 34,
    state: "plan" as const,
    request:
      "Design a clearer boot timeline that remains useful over serial and never hides kernel detail.",
    response:
      "The plan groups milestones without changing the underlying log stream, preserves plain-text output, and adds zero work to the hot path.",
  },
  {
    id: "patient-penguins",
    projectId: "linux",
    title: "Teach penguins to wait patiently",
    branch: "feat/patient-penguins",
    minutesAgo: 52,
    request: "Make delayed work easier to follow without adding noise to the scheduler trace.",
    response:
      "Delayed work now carries a concise reason through the trace, so the wait is legible without changing scheduling behavior.",
    snoozeMinutes: 8 * 60,
  },
  {
    id: "handoff-haptics",
    projectId: "onicode",
    title: "Tune the handoff haptics",
    branch: "feat/handoff-haptics",
    minutesAgo: 5 * 60,
    settled: true,
    request: "Give the desktop-to-phone handoff a haptic that lands with the animation.",
    response:
      "The handoff now taps once as the thread lands and stays silent on failure, so the phone never celebrates a handoff that did not happen.",
  },
  {
    id: "streaming-shell",
    projectId: "react",
    title: "Stream the shell before the data",
    branch: "feat/streaming-shell",
    minutesAgo: 28 * 60,
    settled: true,
    request: "Get the app shell painted before any data request resolves.",
    response:
      "The shell now flushes on first byte and the data boundaries hydrate underneath it, so the first paint no longer waits on the slowest query.",
  },
  {
    id: "quieter-oom",
    projectId: "linux",
    title: "Make the OOM killer explain itself",
    branch: "feat/quieter-oom",
    minutesAgo: 2 * 24 * 60,
    settled: true,
    request: "Make out-of-memory kills legible without adding a single allocation to the hot path.",
    response:
      "Kills now report the winning heuristic and the runner-up alongside the usual dump, assembled entirely from data the path already had.",
  },
] as const;

function minutesBefore(now: number, minutes: number): string {
  return new Date(now - minutes * 60_000).toISOString();
}

async function runGit(workspaceRoot: string, args: ReadonlyArray<string>): Promise<void> {
  await execFile("git", [...args], {
    cwd: workspaceRoot,
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: "Alex Rivera",
      GIT_AUTHOR_EMAIL: "alex@lumen.test",
      GIT_COMMITTER_NAME: "Alex Rivera",
      GIT_COMMITTER_EMAIL: "alex@lumen.test",
    },
  });
}

async function initializeRepository(input: {
  readonly workspaceRoot: string;
  readonly repositoryUrl: string;
  readonly commitMessage: string;
}): Promise<void> {
  await runGit(input.workspaceRoot, ["init", "-b", "main"]);
  await runGit(input.workspaceRoot, ["remote", "add", "origin", input.repositoryUrl]);
  await runGit(input.workspaceRoot, ["add", "."]);
  await runGit(input.workspaceRoot, ["commit", "-m", input.commitMessage]);
}

async function seedOnicodeWorkspace(workspaceRoot: string): Promise<void> {
  await NodeFSP.mkdir(NodePath.join(workspaceRoot, "apps/mobile/src/features/home"), {
    recursive: true,
  });
  await NodeFSP.writeFile(
    NodePath.join(workspaceRoot, "package.json"),
    `${JSON.stringify({ name: "onicode", private: true, scripts: { test: "vp test" } }, null, 2)}\n`,
  );
  await NodeFSP.writeFile(NodePath.join(workspaceRoot, "favicon.svg"), PROJECT_FAVICONS.onicode);
  await NodeFSP.writeFile(
    NodePath.join(workspaceRoot, "apps/mobile/src/features/home/environmentPresence.ts"),
    BASE_ENVIRONMENT_PRESENCE,
  );
  await initializeRepository({
    workspaceRoot,
    repositoryUrl: "https://github.com/aislopware/onicode.git",
    commitMessage: "Show connected environments",
  });
  await runGit(workspaceRoot, ["checkout", "-b", "feat/remote-command-center"]);
  await NodeFSP.writeFile(
    NodePath.join(workspaceRoot, "apps/mobile/src/features/home/environmentPresence.ts"),
    UPDATED_ENVIRONMENT_PRESENCE,
  );
  await NodeFSP.writeFile(
    NodePath.join(workspaceRoot, "apps/mobile/src/features/home/RemoteHandoffCard.tsx"),
    REMOTE_HANDOFF_CARD,
  );
}

async function seedCompanionWorkspace(input: {
  readonly workspaceRoot: string;
  readonly title: string;
  readonly repositoryUrl: string;
  readonly favicon: string;
}): Promise<void> {
  await NodeFSP.mkdir(input.workspaceRoot, { recursive: true });
  await NodeFSP.writeFile(NodePath.join(input.workspaceRoot, "favicon.svg"), input.favicon);
  await NodeFSP.writeFile(
    NodePath.join(input.workspaceRoot, "README.md"),
    `# ${input.title}\n\nSeeded by the Onicode mobile screenshot harness.\n`,
  );
  await initializeRepository({
    workspaceRoot: input.workspaceRoot,
    repositoryUrl: input.repositoryUrl,
    commitMessage: `Seed ${input.title} workspace`,
  });
}

function insertThread(
  database: NodeSqlite.DatabaseSync,
  now: number,
  input: {
    readonly id: string;
    readonly projectId: string;
    readonly title: string;
    readonly branch: string;
    readonly minutesAgo: number;
    readonly state?: "working" | "approval" | "plan";
    readonly settled?: boolean;
    readonly snoozeMinutes?: number;
    readonly workspaceRoot: string;
  },
): void {
  const turnId = `${input.id}-turn`;
  const updatedAt = minutesBefore(now, input.minutesAgo);
  const isWorking = input.state === "working";
  const snoozedUntil =
    input.snoozeMinutes === undefined
      ? null
      : new Date(now + input.snoozeMinutes * 60_000).toISOString();
  const snoozedAt =
    input.snoozeMinutes === undefined
      ? null
      : minutesBefore(now, Math.max(1, Math.floor(input.minutesAgo / 2)));
  database
    .prepare(
      `INSERT INTO projection_threads (
        thread_id, project_id, title, model_selection_json, runtime_mode, interaction_mode,
        branch, worktree_path, latest_turn_id, latest_user_message_at, pending_approval_count,
        pending_user_input_count, has_actionable_proposed_plan, created_at, updated_at,
        archived_at, deleted_at, settled_override, settled_at, snoozed_until, snoozed_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, NULL, NULL, ?, ?, ?, ?)`,
    )
    .run(
      input.id,
      input.projectId,
      input.title,
      MODEL_SELECTION,
      "full-access",
      input.state === "plan" ? "plan" : "default",
      input.branch,
      input.workspaceRoot,
      turnId,
      minutesBefore(now, input.minutesAgo + 1),
      input.state === "approval" ? 1 : 0,
      input.state === "plan" ? 1 : 0,
      minutesBefore(now, input.minutesAgo + 120),
      updatedAt,
      input.settled ? "settled" : null,
      input.settled ? updatedAt : null,
      snoozedUntil,
      snoozedAt,
    );
  database
    .prepare(
      `INSERT INTO projection_thread_sessions (
        thread_id, status, provider_name, provider_instance_id, provider_session_id,
        provider_thread_id, runtime_mode, active_turn_id, last_error, updated_at
      ) VALUES (?, ?, 'Codex', 'codex', NULL, NULL, 'full-access', ?, NULL, ?)`,
    )
    .run(input.id, isWorking ? "running" : "ready", isWorking ? turnId : null, updatedAt);
}

// V2 still owns projection_projects; upsert project rows without deleting them with V1 thread data.
const SEEDED_V1_TABLES = [
  "projection_thread_messages",
  "projection_thread_sessions",
  "projection_threads",
] as const;

const SEEDED_PROJECTION_TABLES = [...SEEDED_V1_TABLES, "projection_projects"] as const;

const SEEDED_THREAD_COLUMNS = ["snoozed_until", "snoozed_at"] as const;

function hasSeedableSchema(dbPath: string): boolean {
  let database: NodeSqlite.DatabaseSync;
  try {
    database = new NodeSqlite.DatabaseSync(dbPath, { readOnly: true });
  } catch {
    return false;
  }
  try {
    const tableCount = database
      .prepare(
        `SELECT COUNT(*) AS count FROM sqlite_master WHERE type = 'table' AND name IN (${SEEDED_PROJECTION_TABLES.map(() => "?").join(", ")})`,
      )
      .get(...SEEDED_PROJECTION_TABLES) as { count: number };
    if (tableCount.count !== SEEDED_PROJECTION_TABLES.length) return false;

    const threadColumns = database.prepare("PRAGMA table_info(projection_threads)").all() as Array<{
      name: string;
    }>;
    const threadColumnNames = new Set(threadColumns.map((column) => column.name));
    return SEEDED_THREAD_COLUMNS.every((column) => threadColumnNames.has(column));
  } catch {
    return false;
  } finally {
    database.close();
  }
}

async function waitForSeedableSchema(dbPath: string, timeoutMs = 60_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (hasSeedableSchema(dbPath)) return;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`The environment server did not migrate ${dbPath} within ${timeoutMs}ms.`);
}

function seedDatabase(
  dbPath: string,
  workspaceRoots: ReadonlyMap<string, string>,
  projects: ReadonlyArray<(typeof SHOWCASE_PROJECTS)[number]>,
  threads: ReadonlyArray<(typeof SHOWCASE_THREADS)[number]>,
  now: number,
): void {
  // The running server writes migrations/projections concurrently; wait for its lock instead of failing SQLITE_BUSY.
  const database = new NodeSqlite.DatabaseSync(dbPath, { timeout: 30_000 });
  try {
    database.exec("BEGIN IMMEDIATE");
    for (const table of SEEDED_V1_TABLES) {
      database.exec(`DELETE FROM ${table}`);
    }
    const insertProject = database.prepare(
      `INSERT OR REPLACE INTO projection_projects (
          project_id, title, workspace_root, default_model_selection_json, scripts_json,
          created_at, updated_at, deleted_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL)`,
    );
    for (const [index, project] of projects.entries()) {
      const workspaceRoot = workspaceRoots.get(project.id);
      if (!workspaceRoot) throw new Error(`Missing workspace root for ${project.id}.`);
      const latestThreadMinutes = Math.min(
        ...threads
          .filter((thread) => thread.projectId === project.id)
          .map((thread) => thread.minutesAgo),
      );
      insertProject.run(
        project.id,
        project.title,
        workspaceRoot,
        MODEL_SELECTION,
        PROJECT_SCRIPTS,
        minutesBefore(now, 60 * 24 * (90 - index * 12)),
        minutesBefore(now, latestThreadMinutes),
      );
    }

    for (const thread of threads) {
      const workspaceRoot = workspaceRoots.get(thread.projectId);
      if (!workspaceRoot) throw new Error(`Missing workspace root for ${thread.projectId}.`);
      insertThread(database, now, {
        ...thread,
        ...("state" in thread ? { state: thread.state } : {}),
        workspaceRoot,
      });
    }

    const insertMessage = database.prepare(
      `INSERT INTO projection_thread_messages (
        message_id, thread_id, turn_id, role, text, is_streaming, attachments_json,
        created_at, updated_at
      ) VALUES (?, ?, ?, ?, ?, 0, NULL, ?, ?)`,
    );
    for (const thread of threads) {
      const turnId = `${thread.id}-turn`;
      const requestTime = minutesBefore(now, thread.minutesAgo + 5);
      insertMessage.run(
        `${thread.id}-request`,
        thread.id,
        turnId,
        "user",
        thread.request,
        requestTime,
        requestTime,
      );
      if (thread.response !== null) {
        const responseTime = minutesBefore(now, thread.minutesAgo);
        insertMessage.run(
          `${thread.id}-answer`,
          thread.id,
          turnId,
          "assistant",
          thread.response,
          responseTime,
          responseTime,
        );
      }
    }

    database.exec("COMMIT");
  } catch (error) {
    // Failed BEGIN or automatic rollback leaves no transaction; preserve the original error if rollback fails.
    try {
      database.exec("ROLLBACK");
    } catch {}
    throw error;
  } finally {
    database.close();
  }
}

export async function seedShowcaseEnvironment(input: {
  readonly baseDir: string;
  readonly projectIds?: ReadonlyArray<string>;
  readonly now?: number;
}): Promise<{ readonly dbPath: string; readonly workspaceRoot: string }> {
  const now = input.now ?? Date.now();
  const selectedProjectIds = new Set(
    input.projectIds ?? SHOWCASE_PROJECTS.map((project) => project.id),
  );
  const projects = SHOWCASE_PROJECTS.filter((project) => selectedProjectIds.has(project.id));
  if (projects.length === 0) throw new Error("At least one showcase project must be selected.");
  const threads = SHOWCASE_THREADS.filter((thread) => selectedProjectIds.has(thread.projectId));
  const workspaceBase = NodePath.join(input.baseDir, "workspace");
  const workspaceRoots = new Map(
    projects.map(
      (project) => [project.id, NodePath.join(workspaceBase, project.directory)] as const,
    ),
  );
  const primaryProject =
    projects.find((project) => project.id === SHOWCASE_PROJECT_ID) ?? projects[0];
  if (!primaryProject) throw new Error("The primary showcase workspace is not configured.");
  const workspaceRoot = workspaceRoots.get(primaryProject.id);
  if (!workspaceRoot) throw new Error("The primary showcase workspace is not configured.");
  const dbPath = NodePath.join(input.baseDir, "userdata", "statev2.sqlite");
  if (primaryProject.id === SHOWCASE_PROJECT_ID) {
    await seedOnicodeWorkspace(workspaceRoot);
  }
  await Promise.all(
    projects
      .filter((project) => project.id !== SHOWCASE_PROJECT_ID)
      .map(async (project) => {
        const projectWorkspaceRoot = workspaceRoots.get(project.id);
        if (!projectWorkspaceRoot) throw new Error(`Missing workspace root for ${project.id}.`);
        await seedCompanionWorkspace({
          workspaceRoot: projectWorkspaceRoot,
          title: project.title,
          repositoryUrl: project.repositoryUrl,
          favicon: project.favicon,
        });
      }),
  );
  // Listening precedes migration completion; wait for the schema before reseeding.
  await waitForSeedableSchema(dbPath);
  seedDatabase(dbPath, workspaceRoots, projects, threads, now);

  const terminalDirectory = NodePath.join(input.baseDir, "userdata", "logs", "terminals");
  if (selectedProjectIds.has(SHOWCASE_PROJECT_ID)) {
    const safeThreadId = Buffer.from(SHOWCASE_THREAD_ID).toString("base64url");
    await NodeFSP.mkdir(terminalDirectory, { recursive: true });
    await NodeFSP.writeFile(
      NodePath.join(terminalDirectory, `terminal_${safeThreadId}.log`),
      SHOWCASE_TERMINAL_BUFFER,
    );
  }
  return { dbPath, workspaceRoot };
}
