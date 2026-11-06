#!/usr/bin/env python3
"""Experimental macOS TRAE session link bridge; install once, then print links."""
import argparse
import json
import pathlib
import re
import subprocess
import tempfile
import urllib.parse
import zipfile


def install(cli):
    source = pathlib.Path(__file__).resolve().with_name("trae-session-link-extension")
    package = json.loads((source / "package.json").read_text())
    manifest = f"""<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
<Metadata><Identity Language="en-US" Id="{package['name']}" Version="{package['version']}" Publisher="{package['publisher']}"/>
<DisplayName>{package['displayName']}</DisplayName><Description xml:space="preserve">{package['description']}</Description>
<Properties><Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.104.0"/></Properties></Metadata>
<Installation><InstallationTarget Id="Microsoft.VisualStudio.Code"/></Installation>
<Dependencies/><Assets><Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/></Assets>
</PackageManifest>"""
    types = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="json" ContentType="application/json"/>
<Default Extension="js" ContentType="application/javascript"/>
<Default Extension="vsixmanifest" ContentType="text/xml"/></Types>"""
    with tempfile.TemporaryDirectory(prefix="session-finder-link-") as temp:
        archive = pathlib.Path(temp) / "trae-session-link.vsix"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as output:
            output.writestr("extension.vsixmanifest", manifest)
            output.writestr("[Content_Types].xml", types)
            for name in ("package.json", "extension.js"):
                output.write(source / name, f"extension/{name}")
        subprocess.run([cli, "--install-extension", str(archive)], check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("conversation", nargs="?", help="24-digit ID or context://history/<ID>")
    parser.add_argument("--install", action="store_true", help="Install the minimal URI bridge")
    parser.add_argument("--open", action="store_true", help="Open the link instead of only printing it")
    parser.add_argument("--cli", default="/Applications/Trae CN.app/Contents/Resources/app/bin/trae-cn")
    parser.add_argument("--project-dir", help="Existing conversation project directory (otherwise resolved locally)")
    parser.add_argument("--finder", default=str(pathlib.Path(__file__).resolve().parent.parent / "session-finder"))
    args = parser.parse_args()
    session_id = None
    if args.conversation:
        session_id = args.conversation.removeprefix("context://history/")
        if not re.fullmatch(r"[a-fA-F0-9]{24}", session_id):
            parser.error("Expected a 24-digit TRAE conversation ID or context://history/<ID>")
    if args.open and session_id is None:
        parser.error("--open requires a conversation ID")
    if args.install:
        install(args.cli)
    if session_id:
        cwd = args.project_dir
        if not cwd:
            try:
                result = subprocess.run(
                    [args.finder, "recent", "--provider", "trae", "--id", session_id, "--json", "--verbose"],
                    check=True, capture_output=True, text=True,
                )
                sessions = json.loads(result.stdout)["sessions"]
                session = next(item for item in sessions if item["id"] == session_id)
                cwd = session.get("cwd")
            except (OSError, subprocess.CalledProcessError, ValueError, KeyError, StopIteration):
                parser.error("Could not resolve the conversation project; provide --project-dir")
        if not cwd or not pathlib.Path(cwd).is_absolute() or not pathlib.Path(cwd).is_dir():
            parser.error("Conversation project must be an existing absolute directory")
        url = "trae-cn://session-finder.trae-session-link/open?" + urllib.parse.urlencode({"id": session_id, "cwd": cwd})
        print(url)
        print(f"\033]8;;{url}\033\\Open TRAE conversation\033]8;;\033\\")
        if args.open:
            subprocess.run(["open", url], check=True)
    elif not args.install:
        parser.error("Provide a conversation ID or --install")


if __name__ == "__main__":
    main()
