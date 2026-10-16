import {
  AppWindow, File, FileArchive, FileAudio, FileCode2, FileImage, FileJson, FileSpreadsheet,
  FileText, FileType, FileVideo, Folder, FolderSymlink,
} from "../lib/icons";
import { Dynamic } from "solid-js/web";
import { kindOf, type Kind } from "../lib/format";
import type { Entry } from "../lib/types";

const ICONS: Record<Kind, any> = {
  folder: Folder, image: FileImage, video: FileVideo, audio: FileAudio, code: FileCode2,
  data: FileJson, text: FileText, doc: FileText, sheet: FileSpreadsheet, pdf: FileText,
  archive: FileArchive, exe: AppWindow, font: FileType, other: File,
};

export default function FileIcon(props: { entry: Pick<Entry, "isDir" | "ext" | "isSymlink">; size?: number }) {
  const kind = () => kindOf(props.entry);
  const icon = () => (props.entry.isDir && props.entry.isSymlink ? FolderSymlink : ICONS[kind()]);
  return (
    <span class={`ficon k-${kind()}`}>
      <Dynamic
        component={icon()}
        size={props.size ?? 16}
        stroke-width={props.size && props.size > 40 ? 1.25 : 1.75}
        fill={kind() === "folder" ? "currentColor" : "none"}
        fill-opacity={kind() === "folder" ? 0.22 : 0}
      />
    </span>
  );
}
