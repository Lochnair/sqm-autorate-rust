pipeline {
	agent any

	stages {
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
                    }
                }

				stages {
					stage('Build') {
						steps {
							sh '''
								env CARGO_HOME="$(pwd)/.cargo" cargo build \
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