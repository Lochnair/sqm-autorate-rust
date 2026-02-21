pipeline {
	agent any

	stages {
		stage('Main') {
			matrix {
				axes {
					axis {
						name 'TARGET'
						values 'x86_64-musl', 'i686-musl', 'arm-musleabi', 'arm-musleabihf', 'armv7-musleabi', 'armv7-musleabihf', 'aarch64-musl', 'mips-musl', 'mipsel-musl', 'mips64-muslabi64', 'mips64el-muslabi64', 'riscv64gc-musl'
					}
				}

				agent {
                    docker {
                        image "ghcr.io/rust-cross/rust-musl-cross:${TARGET}"
						alwaysPull true
                    }
                }

				stages {
					stage('Build') {
						steps {
							sh '''
								# 1. Set CARGO_HOME to a writable directory in the workspace
								export CARGO_HOME="$(pwd)/.cargo"
								mkdir -p "$CARGO_HOME"
								
								# 2. Add local bin to PATH (for tools you might install)
								export PATH="$(pwd)/.local/bin:${PATH}"

								# 3. CRITICAL: Copy the cross-compilation config from the image's default location
								#    The image stores the linker configuration in /root/.cargo/config.toml.
								#    Without this, Cargo defaults to the host linker (cc) and fails.
								if [ -f /root/.cargo/config.toml ]; then
									cp /root/.cargo/config.toml "$CARGO_HOME/config.toml"
								else
									echo "WARNING: /root/.cargo/config.toml not found. Build may fail."
								fi

								# 4. Debug: Verify the config is present and correct
								echo "--- Active Cargo Configuration ---"
								cat "$CARGO_HOME/config.toml"
								echo "----------------------------------"

								# 5. Check Toolchain & Build
                                # We grep for "nightly" in the version string (e.g., "rustc 1.95.0-nightly...")
                                if rustc -V | grep -q "nightly"; then
                                    echo "Detected Nightly toolchain. Enabling '-Z build-std' to support panic=abort..."
                                    
                                    # Ensure rust-src is installed (needed for build-std)
                                    # We use '|| true' so it doesn't fail if already installed or network flakes
                                    rustup component add rust-src || true
                                    
                                    cargo build --release -Z build-std=std,panic_abort
                                else
                                    echo "Detected Stable toolchain. Building with standard pre-compiled library..."
                                    cargo build --release
                                fi
							'''
						}
					}

					stage('Archive artifact') {
						steps {
							sh '''
							ls -l target
							find -name sqm-autorate-rust
							'''
							/*dir("target/${TARGET}/release") {
								sh "cp -v sqm-autorate-rust sqm-autorate-rust-${TARGET}"
								archiveArtifacts artifacts: "sqm-autorate-rust-${TARGET}", fingerprint: true, onlyIfSuccessful: true
							}*/
						}
					}
				}
			}
		}
	}
}
