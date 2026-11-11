# Turns the addresses in Ferret's log (an installed copy's "ferret.exe+0x1cc1776" lines, from the
# watchdog: where the window was stuck) into function names, files and lines, with the symbols
# release.sh keeps for each release (ferret-gpui/target/symbols/<version>: ferret.exe and
# ferret_gpui.pdb).
#
#   scripts/symbolize-log.ps1                       the installed copy's log, its own version
#   scripts/symbolize-log.ps1 -Log <file> -Version 0.10.1
param(
    [string]$Log = "$env:LOCALAPPDATA\com.ferret.app\logs\ferret.log",
    [string]$Version = "",
    [int]$Tail = 400
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $Version) {
    # The last "Ferret x.y.z started" line says which build wrote it.
    $started = Select-String -Path $Log -Pattern 'Ferret (\d+\.\d+\.\d+) started' | Select-Object -Last 1
    if (-not $started) { throw "The log doesn't say which version wrote it: pass -Version." }
    $Version = $started.Matches[0].Groups[1].Value
}
$dir = Join-Path $root "ferret-gpui\target\symbols\$Version"
$exe = Join-Path $dir "ferret.exe"
if (-not (Test-Path $exe)) { throw "No symbols kept for $Version (expected ${dir}; release.sh keeps them for each release)." }

Add-Type @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class Dbg {
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern bool SymInitializeW(IntPtr p, string path, bool invade);
    [DllImport("dbghelp.dll", SetLastError = true)] public static extern uint SymSetOptions(uint o);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern ulong SymLoadModuleExW(IntPtr p, IntPtr file, string image, string module, ulong baseAddr, uint size, IntPtr data, uint flags);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern bool SymFromAddrW(IntPtr p, ulong addr, out ulong disp, IntPtr info);
    [DllImport("dbghelp.dll", CharSet = CharSet.Unicode, SetLastError = true)] public static extern bool SymGetLineFromAddrW64(IntPtr p, ulong addr, out uint disp, ref LINE line);
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] public struct LINE { public uint SizeOfStruct; public IntPtr Key; public uint LineNumber; public IntPtr FileName; public ulong Address; }
    public static string Name(IntPtr p, ulong addr) {
        int size = 88 + 2 * 1024;
        IntPtr buf = Marshal.AllocHGlobal(size);
        try {
            for (int i = 0; i < size; i++) Marshal.WriteByte(buf, i, 0);
            Marshal.WriteInt32(buf, 0, 88);          // SizeOfStruct
            Marshal.WriteInt32(buf, 80, 1024);       // MaxNameLen
            ulong d;
            if (!SymFromAddrW(p, addr, out d, buf)) return null;
            int len = Marshal.ReadInt32(buf, 76);    // NameLen
            string name = Marshal.PtrToStringUni(new IntPtr(buf.ToInt64() + 84), Math.Min(len, 1024));
            LINE l = new LINE(); l.SizeOfStruct = (uint)Marshal.SizeOf(typeof(LINE)); uint d32;
            if (SymGetLineFromAddrW64(p, addr, out d32, ref l)) {
                string file = Marshal.PtrToStringUni(l.FileName);
                string[] parts = file.Split('\\', '/');
                string shortFile = parts.Length >= 2 ? parts[parts.Length - 2] + "/" + parts[parts.Length - 1] : file;
                name += " (" + shortFile + ":" + l.LineNumber + ")";
            }
            return name;
        } finally { Marshal.FreeHGlobal(buf); }
    }
}
'@
$proc = [IntPtr]0x0FE77E  # any unique value: dbghelp only needs a handle-like key here
[Dbg]::SymSetOptions(0x2 -bor 0x10) | Out-Null   # UNDNAME | LOAD_LINES
if (-not [Dbg]::SymInitializeW($proc, $dir, $false)) { throw "dbghelp didn't start" }
$base = [uint64]0x10000000
if ([Dbg]::SymLoadModuleExW($proc, [IntPtr]::Zero, $exe, "ferret", $base, 0, [IntPtr]::Zero, 0) -eq 0) { throw "couldn't load $exe's symbols" }

Get-Content $Log -Tail $Tail | ForEach-Object {
    if ($_ -match '^(\s*)(ferret(?:-gpui)?\.exe)\+0x([0-9a-f]+)(.*)$' -and $matches[4].Trim() -eq '') {
        $rva = [Convert]::ToUInt64($matches[3], 16)
        $name = [Dbg]::Name($proc, $base + $rva)
        if ($name) { "$($matches[1])$($matches[2])+0x$($matches[3]) $name" } else { $_ }
    } else { $_ }
}
