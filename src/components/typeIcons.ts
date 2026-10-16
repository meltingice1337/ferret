// Icons for file types (search chips, help, the folder filter's type menu).
import { AlignLeft, AppWindow, Archive, Braces, Code, FileText, Film, Folder, Image, Music, Shapes, Sheet, Type } from "../lib/icons";

const ICONS: Record<string, typeof Folder> = {
  folder: Folder, image: Image, video: Film, audio: Music, doc: FileText, text: AlignLeft, code: Code,
  archive: Archive, app: AppWindow, data: Braces, font: Type, pdf: FileText, sheet: Sheet, exe: AppWindow,
};

export const typeIcon = (id: string) => ICONS[id] ?? Shapes;
