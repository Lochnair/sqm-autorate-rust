def targetMapping = [
	"mips-unknown-linux-musl": "mips-linux-muslsf",
	"mips64-unknown-linux-muslabi64": "mips64-linux-musl",
	"mips64el-unknown-linux-muslabi64": "mips64el-linux-musl",
	"mipsel-unknown-linux-musl": "mipsel-linux-muslsf",
	"x86_64-unknown-linux-musl": "x86_64-linux-musl"
]

def getGCCTarget(rust_target) {
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
						values 'mips-unknown-linux-musl', 'mips64-unknown-linux-muslabi64', 'mips64el-unknown-linux-muslabi64', 'mipsel-unknown-linux-musl', 'x86_64-unknown-linux-musl'
					}
				}

				environment {
					GCC_TARGET = getGCCTarget(TARGET)
					CC = '${GCC_TARGET}-gcc'
					PATH = "${WORKSPACE}/${GCC_TARGET}-cross:${env.HOME}/.cargo/bin:${env.PATH}"
					TOOLCHAIN_URL = 'https://musl.cc/${GCC_TARGET}-cross.tgz'
				}

				stages {
					stage('Download GCC toolchain') {
						steps {
							httpRequest outputFile: GCC_TARGET + '-cross.tgz', url: TOOLCHAIN_URL
							sh 'mkdir ' + GCC_TARGET + '-cross'
							sh 'tar -x -z -f ' + GCC_TARGET + '-cross.tgz'
						}
					}

					stage('Build') {
						steps {
							sh "rustc --version"
						}
					}
				}
			}
		}
	}
}