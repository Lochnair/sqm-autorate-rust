def getArch(target) {
	return target.tokenize('-')[0]
}

def getGCCTarget(rust_target) {
	targetMapping = [
    	"mips-unknown-linux-musl": "mips-linux-muslsf",
    	"mips64-unknown-linux-muslabi64": "mips64-linux-musl",
    	"mips64el-unknown-linux-muslabi64": "mips64el-linux-musl",
    	"mipsel-unknown-linux-musl": "mipsel-linux-muslsf",
    	"x86_64-unknown-linux-musl": "x86_64-linux-musl"
    ]

	return targetMapping[rust_target]
}

pipeline {
	agent any

	stages {
		stage('Main') {
			matrix {
				axes {
					axis {
						name 'TARGET'
						//values 'mips-unknown-linux-musl', 'mips64-unknown-linux-muslabi64', 'mips64el-unknown-linux-muslabi64', 'mipsel-unknown-linux-musl', 'x86_64-unknown-linux-musl'
						values 'mips-unknown-linux-musl'
					}
				}

				environment {
					GCC_TARGET = getGCCTarget(TARGET)
					TARGET_ARCH = getArch(GCC_TARGET)
					CC = "${GCC_TARGET}-gcc"
					PATH = "${WORKSPACE}/${GCC_TARGET}-cross/bin:${env.HOME}/.cargo/bin:${env.PATH}"
					TOOLCHAIN_URL = "https://musl.cc/${GCC_TARGET}-cross.tgz"
				}

				stages {
					stage('Install Rust target') {
						when {
							expression {
								def res = sh(script: "rustup target list --installed", returnStdout: true)

								if (res.contains(TARGET)) {
                                    return false
                                } else {
                                    return true
                                }
							}
						}

						steps {
							sh "rustup target add ${TARGET}"
						}
					}
					stage('Download GCC toolchain') {
						when {
							expression {
								def res = sh(script: "${CC} -v", returnStatus: true)

                                if (res > 0) {
                                    return true
                                } else {
                                    return false
                                }
							}
						}
						steps {
							sh "wget -q ${TOOLCHAIN_URL}"
							sh 'mkdir ' + GCC_TARGET + '-cross'
							sh 'tar -x -z -f ' + GCC_TARGET + '-cross.tgz'
							sh 'rm -vf ${GCC_TARGET}-cross.tgz'
						}
					}

					stage('Build') {
						steps {
							sh "cargo build --release --target ${TARGET}"
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