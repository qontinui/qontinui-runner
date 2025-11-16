#!/bin/bash
# Install all Tauri dependencies for Ubuntu/Debian

echo "Installing Tauri system dependencies..."

sudo apt update

sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libwebkit2gtk-4.0-dev \
  build-essential \
  curl \
  wget \
  file \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev \
  libjavascriptcoregtk-4.0-dev \
  pkg-config

echo "Done! You can now run: npm run tauri dev"
