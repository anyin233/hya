/**
 * Self-sandboxing launcher for an extension process; no helper binary.
 *
 *   bun confine.ts <readable path>... -- <program> <args>...
 *
 * It restricts its own process with the operating system's sandbox API and
 * then `execve`s `program`, so every thread of the new image inherits the
 * restriction (Landlock and seccomp bind the calling thread only, and Bun is
 * multi-threaded before user code runs).
 *
 * - Linux: `no_new_privs`, Landlock (read: system libraries and the given
 *   paths; execute: the bun binary's directory and its ELF interpreter; its own
 *   `/proc/<pid>`; no writes; no TCP; signals and abstract sockets scoped) and a
 *   seccomp filter that refuses `socket`/`socketpair` (UDP, raw, netlink, unix).
 * - macOS: `sandbox_init` (libsandbox, what sandbox-exec calls) with a
 *   deny-default profile: reads of system libraries and the given paths, exec
 *   of the bun binary only, no writes, no network.
 *
 * Exit status 97 with a reason on stderr: the sandbox is unavailable here.
 * The environment passed to `program` is empty.
 */
import { dlopen, FFIType, ptr, toArrayBuffer, CString } from "bun:ffi"
import { openSync, readFileSync, realpathSync, statSync } from "node:fs"
import { dirname } from "node:path"

const separator = process.argv.indexOf("--")
const readable = process.argv.slice(2, separator).map((path) => realpathSync(path))
const program = process.argv.slice(separator + 1)
if (separator < 0 || program.length === 0) unavailable("usage: confine.ts <readable>... -- <program> <args>...")
const executable = realpathSync(program[0]!)

function unavailable(reason: string): never {
  process.stderr.write(`${reason}\n`)
  process.exit(97)
}

/** NUL-terminated argv/envp arrays for execve; `keep` holds the buffers alive. */
function cArray(values: readonly string[], keep: Buffer[]): Buffer {
  const array = Buffer.alloc((values.length + 1) * 8)
  values.forEach((value, index) => {
    const bytes = Buffer.from(`${value}\0`)
    keep.push(bytes)
    array.writeBigUInt64LE(BigInt(ptr(bytes)), index * 8)
  })
  return array
}

function confineLinux(): (path: Buffer, argv: Buffer, envp: Buffer) => number {
  const libc = dlopen("libc.so.6", {
    syscall: { args: [FFIType.i64, FFIType.u64, FFIType.u64, FFIType.u64, FFIType.u64], returns: FFIType.i64 },
    prctl: { args: [FFIType.i32, FFIType.u64, FFIType.u64, FFIType.u64, FFIType.u64], returns: FFIType.i32 },
    execve: { args: [FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
    __errno_location: { args: [], returns: FFIType.ptr },
  })
  const errno = () => new Int32Array(toArrayBuffer(libc.symbols.__errno_location()!, 0, 4))[0]
  // Generic syscall numbers (same on x86_64 and aarch64 for Landlock).
  const CREATE = 444n, ADD_RULE = 445n, RESTRICT = 446n
  const abi = Number(libc.symbols.syscall(CREATE, 0n, 0n, 1n /* LANDLOCK_CREATE_RULESET_VERSION */, 0n))
  if (abi < 1) unavailable(`Landlock is not available in this kernel (errno ${errno()}); enable CONFIG_SECURITY_LANDLOCK and the landlock LSM`)

  // Filesystem rights by ABI: v1 has 13; v2 REFER; v3 TRUNCATE; v5 IOCTL_DEV.
  let handledFs = (1n << 13n) - 1n
  if (abi >= 2) handledFs |= 1n << 13n
  if (abi >= 3) handledFs |= 1n << 14n
  if (abi >= 5) handledFs |= 1n << 15n
  const attr = Buffer.alloc(24)
  attr.writeBigUInt64LE(handledFs, 0)
  attr.writeBigUInt64LE(abi >= 4 ? 3n : 0n, 8) // TCP bind and connect: no rule, so none allowed
  attr.writeBigUInt64LE(abi >= 6 ? 3n : 0n, 16) // abstract unix sockets and signals stay inside the domain
  const ruleset = Number(libc.symbols.syscall(CREATE, BigInt(ptr(attr)), BigInt(abi >= 6 ? 24 : abi >= 4 ? 16 : 8), 0n, 0n))
  if (ruleset < 0) unavailable(`landlock_create_ruleset failed (errno ${errno()})`)

  const EXECUTE = 1n, WRITE_FILE = 2n, READ_FILE = 4n, READ_DIR = 8n
  const FILE_RIGHTS = EXECUTE | WRITE_FILE | READ_FILE | (1n << 14n) | (1n << 15n)
  const O_PATH = 0o10000000, O_CLOEXEC = 0o2000000
  const allow = (path: string, access: bigint): void => {
    let fd: number
    try { fd = openSync(path, O_PATH | O_CLOEXEC) } catch { return } // optional system paths (/lib64, …)
    const rule = Buffer.alloc(12) // struct landlock_path_beneath_attr (packed)
    rule.writeBigUInt64LE(statSync(path).isDirectory() ? access : access & FILE_RIGHTS, 0)
    rule.writeInt32LE(fd, 8)
    if (libc.symbols.syscall(ADD_RULE, BigInt(ruleset), 1n /* PATH_BENEATH */, BigInt(ptr(rule)), 0n) !== 0n) unavailable(`landlock_add_rule ${path} failed (errno ${errno()})`)
  }
  for (const path of ["/usr", "/lib", "/lib64", "/lib32", "/bin", "/etc/ld.so.cache", ...readable]) allow(path, READ_FILE | READ_DIR)
  allow(dirname(executable), READ_FILE | READ_DIR | EXECUTE)
  const interpreter = elfInterpreter(executable)
  if (interpreter) { allow(interpreter, READ_FILE | EXECUTE); allow(realpathSync(interpreter), READ_FILE | EXECUTE) }
  allow(`/proc/${process.pid}`, READ_FILE | READ_DIR) // its own /proc only; the pid survives execve
  allow("/dev/null", READ_FILE | WRITE_FILE)
  allow("/dev/urandom", READ_FILE)

  const arch = process.arch === "arm64" ? { audit: 0xc00000b7, socket: 198, socketpair: 199 } : process.arch === "x64" ? { audit: 0xc000003e, socket: 41, socketpair: 53 } : unavailable(`seccomp filter not defined for ${process.arch}`)
  const instruction = (code: number, jt: number, jf: number, k: number) => {
    const bytes = Buffer.alloc(8)
    bytes.writeUInt16LE(code, 0); bytes[2] = jt; bytes[3] = jf; bytes.writeUInt32LE(k >>> 0, 4)
    return bytes
  }
  const LOAD = 0x20, JEQ = 0x15, RET = 0x06
  const filter = Buffer.concat([
    instruction(LOAD, 0, 0, 4), instruction(JEQ, 1, 0, arch.audit), instruction(RET, 0, 0, 0x80000000), // foreign arch: kill
    instruction(LOAD, 0, 0, 0),
    instruction(JEQ, 2, 0, arch.socket), instruction(JEQ, 1, 0, arch.socketpair),
    instruction(RET, 0, 0, 0x7fff0000), // allow
    instruction(RET, 0, 0, 0x00050001), // EPERM
  ])
  const fprog = Buffer.alloc(16) // struct sock_fprog
  fprog.writeUInt16LE(filter.length / 8, 0)
  fprog.writeBigUInt64LE(BigInt(ptr(filter)), 8)

  if (libc.symbols.prctl(38 /* PR_SET_NO_NEW_PRIVS */, 1n, 0n, 0n, 0n) !== 0) unavailable(`no_new_privs failed (errno ${errno()})`)
  if (libc.symbols.syscall(RESTRICT, BigInt(ruleset), 0n, 0n, 0n) !== 0n) unavailable(`landlock_restrict_self failed (errno ${errno()})`)
  if (libc.symbols.prctl(22 /* PR_SET_SECCOMP */, 2n /* FILTER */, BigInt(ptr(fprog)), 0n, 0n) !== 0) unavailable(`seccomp filter failed (errno ${errno()})`)
  return (path, argv, envp) => { libc.symbols.execve(ptr(path), ptr(argv), ptr(envp)); return errno() ?? -1 }
}

/** PT_INTERP of a 64-bit little-endian ELF: the loader the kernel opens with exec rights. */
function elfInterpreter(path: string): string | undefined {
  const elf = readFileSync(path)
  if (elf.readUInt32BE(0) !== 0x7f454c46) return undefined
  const phoff = Number(elf.readBigUInt64LE(0x20)), size = elf.readUInt16LE(0x36), count = elf.readUInt16LE(0x38)
  for (let index = 0; index < count; index++) {
    const at = phoff + index * size
    if (elf.readUInt32LE(at) !== 3) continue
    const offset = Number(elf.readBigUInt64LE(at + 8)), length = Number(elf.readBigUInt64LE(at + 32))
    return elf.subarray(offset, offset + length - 1).toString()
  }
  return undefined
}

function confineDarwin(): (path: Buffer, argv: Buffer, envp: Buffer) => number {
  let sandbox
  try {
    sandbox = dlopen("/usr/lib/libsandbox.1.dylib", { sandbox_init: { args: [FFIType.ptr, FFIType.u64, FFIType.ptr], returns: FFIType.i32 } })
  } catch (error) {
    unavailable(`libsandbox unavailable: ${error instanceof Error ? error.message : String(error)}`)
  }
  const libc = dlopen("/usr/lib/libSystem.B.dylib", {
    execve: { args: [FFIType.ptr, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
    __error: { args: [], returns: FFIType.ptr },
  })
  const paths = ["/usr/lib", "/usr/share", "/System", "/Library/Apple", "/private/var/db", "/dev", dirname(dirname(executable)), ...readable]
    .map((path) => `(subpath ${JSON.stringify(path)})`).join(" ")
  const profile = [
    "(version 1)", "(deny default)",
    `(allow process-exec (literal ${JSON.stringify(executable)}))`,
    "(allow process-fork)", "(allow signal (target self))", "(allow sysctl-read)", "(allow mach-lookup)", "(allow ipc-posix-shm)",
    "(allow file-read-metadata)", `(allow file-read* (literal "/") ${paths})`,
    '(allow file-write* (literal "/dev/null"))',
  ].join("")
  const error = Buffer.alloc(8)
  if (sandbox.symbols.sandbox_init(ptr(Buffer.from(`${profile}\0`)), 0n, ptr(error)) !== 0) {
    const message = error.readBigUInt64LE(0)
    unavailable(`sandbox_init failed: ${message ? new CString(Number(message)).toString() : "unknown error"}`)
  }
  return (path, argv, envp) => { libc.symbols.execve(ptr(path), ptr(argv), ptr(envp)); return new Int32Array(toArrayBuffer(libc.symbols.__error()!, 0, 4))[0] ?? -1 }
}

const execve = process.platform === "linux" ? confineLinux() : process.platform === "darwin" ? confineDarwin() : unavailable(`no OS sandbox for ${process.platform}`)
const keep: Buffer[] = []
const path = Buffer.from(`${executable}\0`)
const failed = execve(path, cArray(program, keep), cArray([], keep))
process.stderr.write(`execve ${executable} failed (errno ${failed})\n`)
process.exit(98)
