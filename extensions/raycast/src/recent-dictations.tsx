import { Action, ActionPanel, Icon, List, getPreferenceValues, showToast, Toast } from "@raycast/api";
import { useEffect, useState } from "react";
import { type Dictation, historyFile, output, readHistory } from "./history";

interface Preferences {
  dataDir?: string;
  primaryAction?: "paste" | "copy";
}

function relativeTime(at: Date): string {
  const s = Math.round((Date.now() - at.getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  if (s < 7 * 86400) return `${Math.floor(s / 86400)}d ago`;
  return at.toLocaleDateString();
}

function firstLine(s: string): string {
  const line = s.trim().split("\n")[0] ?? "";
  return line.length > 90 ? `${line.slice(0, 89)}…` : line || "(empty)";
}

export default function RecentDictations() {
  const prefs = getPreferenceValues<Preferences>();
  const [items, setItems] = useState<Dictation[]>();
  const [showDetail, setShowDetail] = useState(true);

  const load = () => {
    try {
      setItems(readHistory(prefs.dataDir));
    } catch (e) {
      setItems([]);
      showToast({ style: Toast.Style.Failure, title: "Couldn't read Stenly history", message: String(e) });
    }
  };
  useEffect(load, []);

  return (
    <List isLoading={items === undefined} isShowingDetail={showDetail && !!items?.length} searchBarPlaceholder="Search dictations">
      <List.EmptyView
        icon={Icon.Microphone}
        title="No dictations yet"
        description={`Stenly's history is empty or missing:\n${historyFile(prefs.dataDir)}`}
      />
      {items?.map((d) => {
        const text = output(d);
        const at = new Date(d.at);
        const accessories: List.Item.Accessory[] = showDetail
          ? []
          : [...(d.app ? [{ tag: d.app }] : []), { text: relativeTime(at), tooltip: at.toLocaleString() }];
        const paste = <Action.Paste key="paste" title="Paste Dictation" content={text} />;
        const copy = <Action.CopyToClipboard key="copy" title="Copy Dictation" content={text} />;
        return (
          <List.Item
            key={d.at}
            icon={Icon.Microphone}
            title={firstLine(text)}
            subtitle={showDetail ? relativeTime(at) : undefined}
            keywords={[...text.split(/\s+/).slice(0, 50), ...(d.app ? [d.app] : [])]}
            accessories={accessories}
            detail={
              <List.Item.Detail
                markdown={text}
                metadata={
                  <List.Item.Detail.Metadata>
                    <List.Item.Detail.Metadata.Label title="When" text={at.toLocaleString()} />
                    {d.app ? <List.Item.Detail.Metadata.Label title="App" text={d.app} /> : null}
                    <List.Item.Detail.Metadata.Label title="Duration" text={`${d.duration.toFixed(1)}s`} />
                    <List.Item.Detail.Metadata.Label title="Words" text={String(text.split(/\s+/).filter(Boolean).length)} />
                    {d.enhanced ? <List.Item.Detail.Metadata.Label title="AI Clean-up" text="Yes" /> : null}
                  </List.Item.Detail.Metadata>
                }
              />
            }
            actions={
              <ActionPanel>
                <ActionPanel.Section>{prefs.primaryAction === "copy" ? [copy, paste] : [paste, copy]}</ActionPanel.Section>
                <ActionPanel.Section>
                  {d.enhanced && d.enhanced !== d.text ? (
                    <Action.CopyToClipboard
                      title="Copy Raw Transcript"
                      content={d.text}
                      shortcut={{ modifiers: ["cmd", "shift"], key: "c" }}
                    />
                  ) : null}
                  <Action
                    title={showDetail ? "Hide Details" : "Show Details"}
                    icon={Icon.Sidebar}
                    shortcut={{ modifiers: ["cmd"], key: "d" }}
                    onAction={() => setShowDetail((v) => !v)}
                  />
                  <Action
                    title="Reload"
                    icon={Icon.ArrowClockwise}
                    shortcut={{ modifiers: ["cmd"], key: "r" }}
                    onAction={load}
                  />
                </ActionPanel.Section>
              </ActionPanel>
            }
          />
        );
      })}
    </List>
  );
}
