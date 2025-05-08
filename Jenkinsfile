def getArch(target) {
	return target.tokenize('-')[0]
}

def getGCCTarget(rust_target) {
	def targetMapping = [
		"aarch64-unknown-linux-musl": "aarch64-linux-musl",
		"arm-unknown-linux-musleabi": "arm-linux-musleabi",
		"armv7-unknown-linux-musleabi": "armv7m-linux-musleabi",
		"armv7-unknown-linux-musleabihf": "armv7l-linux-musleabihf",
    	"mips-unknown-linux-musl": "mips-linux-muslsf",
    	"mips64-unknown-linux-muslabi64": "mips64-linux-musl",
    	"mips64el-unknown-linux-muslabi64": "mips64el-linux-musl",
    	"mipsel-unknown-linux-musl": "mipsel-linux-muslsf",
    	"x86_64-unknown-linux-musl": "x86_64-linux-musl"
    ]

	return targetMapping[rust_target]
}

// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER
def getLinkerEnv(target) {
	return 'CARGO_TARGET_' + target.replaceAll('-', '_').toUpperCase() + '_LINKER'
}

pipeline {
	agent { dockerfile true }

	stages {
		stage('Main') {
			matrix {
				axes {
					axis {
						name 'TARGET'
						values 'aarch64-unknown-linux-musl', 'arm-unknown-linux-musleabi', 'armv7-unknown-linux-musleabi', 'armv7-unknown-linux-musleabihf','mips-unknown-linux-musl', 'mips64-unknown-linux-muslabi64', 'mips64el-unknown-linux-muslabi64', 'mipsel-unknown-linux-musl', 'x86_64-unknown-linux-musl'
					}
				}

				environment {
					GCC_TARGET = getGCCTarget(TARGET)
					TARGET_ARCH = getArch(GCC_TARGET)
					CC = "${GCC_TARGET}-gcc"
					LINKER_ENV_KEY = getLinkerEnv(TARGET)
					PATH_CC = "/opt/${GCC_TARGET}-cross/bin:/home/sdk/.cargo/bin"
					RUSTFLAGS = "-C target-feature=+crt-static"
				}

				stages {
					stage('Build') {
						steps {
							sh '''
								export PATH="$PATH_CC:$PATH"
								cargo +nightly build \
									-Z build-std=std,panic_abort \
									-Z build-std-features="optimize_for_size" \
									--target $TARGET --release
								'''
						}
					}

					stage('Archive artifact') {
						steps {
							dir("target/${TARGET}/release") {
								sh "cp -v sqm-autorate-rust sqm-autorate-rust-${TARGET_ARCH}"
								archiveArtifacts artifacts: "sqm-autorate-rust-${TARGET_ARCH}", fingerprint: true, onlyIfSuccessful: true
							}
						}
					}
				}
			}
		}
	}
}