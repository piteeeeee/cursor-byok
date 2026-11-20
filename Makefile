APP_NAME := Cursor BYOK Lite
APP_BUNDLE := target/release/bundle/$(APP_NAME).app
BINARY := target/release/cursor-byok-lite

.PHONY: check dev-server build-server build-app dmg

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace --all-targets

dev-server:
	cargo run --package cursor-server --bin cursor-server

build-server:
	cargo build --release --package cursor-server --bin cursor-server

# Assembles the arm64 menu bar app bundle from the release binary. Signing and
# installation stay a separate, explicit step.
build-app:
	cargo build --release --package cursor-byok-lite
	rm -rf "$(APP_BUNDLE)"
	install -d "$(APP_BUNDLE)/Contents/MacOS" "$(APP_BUNDLE)/Contents/Resources"
	install -m 755 "$(BINARY)" "$(APP_BUNDLE)/Contents/MacOS/cursor-byok-lite"
	install -m 644 apps/menubar/Info.plist "$(APP_BUNDLE)/Contents/Info.plist"
	printf 'APPL????' > "$(APP_BUNDLE)/Contents/PkgInfo"
	codesign --force --deep --sign - "$(APP_BUNDLE)"
	@echo "built $(APP_BUNDLE)"

dmg: build-app
	hdiutil create -volname "$(APP_NAME)" -srcfolder "$(APP_BUNDLE)" -ov -format UDZO "target/release/$(APP_NAME).dmg"
