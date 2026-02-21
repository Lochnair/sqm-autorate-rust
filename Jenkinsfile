pipeline {
	agent any

	stages {
		stage('Preparation') {
			steps {
				sh '''
				    mkdir -p .local
					curl -L -s https://github.com/rui314/mold/releases/download/v2.40.4/mold-2.40.4-x86_64-linux.tar.gz | tar -xzv --strip-components=1 -C .local/
				'''
			}
		}
		stage('Main') {
			matrix {
				axes {
					axis {
						name 'TARGET'
						values 'x86_64-musl', 'i686-musl', 'arm-musleabi', 'arm-musleabihf', 'armv7-musleabi', 'armv7-musleabihf', 'aarch64-musl', 'mips-musl', 'mipsel-musl', 'mips64-musl', 'mips64el-musl', 'mips64-muslabi64', 'mips64el-muslabi64', 'riscv64gc-musl'
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
								export CARGO_HOME="$(pwd)/.cargo"
								export PATH="$(pwd)/.local/bin:${PATH}"
								mold -V
								clang -v
								cargo -V
								rustc -V
								cargo build \
									--release
								'''
						}
					}

					stage('Archive artifact') {
						steps {
							sh 'echo TODO'
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
