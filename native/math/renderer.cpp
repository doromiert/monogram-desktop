// SPDX-License-Identifier: GPL-3.0-or-later
// Bounded native typesetting adapter. Only launched inside the networkless sandbox.
#include "latex.h"
#include "core/formula.h"
#include "platform/cairo/graphic_cairo.h"
#include "utils/utf.h"
#include <pangomm/init.h>
#include <cerrno>
#include <cstdio>
#include <iostream>
#include <memory>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
static int limit(int resource,rlim_t value){struct rlimit v={value,value};return setrlimit(resource,&v)==0;}
#define DENY(call) BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,SYS_##call,0,1),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|EPERM)
static int restrict_calls(void){
#if defined(__x86_64__)
 const unsigned int arch=AUDIT_ARCH_X86_64;
#elif defined(__aarch64__)
 const unsigned int arch=AUDIT_ARCH_AARCH64;
#else
 return 0;
#endif
 struct sock_filter instructions[]={
  BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,arch)),
  BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,arch,1,0),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
  BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,nr)),
  BPF_JUMP(BPF_JMP|BPF_JGE|BPF_K,0x40000000,0,1),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
  DENY(clone),DENY(clone3),DENY(execve),DENY(execveat),DENY(socket),DENY(socketpair),DENY(ptrace),DENY(process_vm_readv),DENY(process_vm_writev),DENY(setns),DENY(unshare),DENY(mount),DENY(umount2),
#ifdef SYS_fork
  DENY(fork),
#endif
#ifdef SYS_vfork
  DENY(vfork),
#endif
  BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW)
 };
 struct sock_fprog program={sizeof instructions/sizeof instructions[0],instructions};
 return prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&program)==0;
}

int main(int argc, char** argv) {
  if (argc != 3 || !limit(RLIMIT_AS,512u*1024u*1024u) || !limit(RLIMIT_STACK,8u*1024u*1024u)
      || !limit(RLIMIT_CORE,0) || !limit(RLIMIT_FSIZE,0) || !limit(RLIMIT_NOFILE,64)
      || prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0) != 0 || !restrict_calls()) return 2;
  umask(077); alarm(8);
  try {
    std::string source; char c;
    while (source.size() <= 32768 && std::cin.get(c)) source += c;
    if (source.empty() || source.size() > 32768 || source.find('\0') != std::string::npos) return 3;
    for (const char* name : {"includegraphics","input","include","write","read","newcommand","renewcommand","newenvironment","def","catcode","special"})
      if (source.find(std::string("\\") + name) != std::string::npos) return 3;
    auto wide = tex::utf82wide(source);
    if (wide.size() > 8192) return 3;
    unsigned long foreground = std::stoul(argv[2], nullptr, 16);
    Pango::init();
    tex::LaTeX::init(MONOGRAM_MATH_RES);
    tex::Formula formula(wide);
    tex::TeXRenderBuilder builder;
    std::unique_ptr<tex::TeXRender> render(builder.setStyle(std::string(argv[1]) == "display" ? tex::TexStyle::display : tex::TexStyle::text)
      .setTextSize(36).setForeground(static_cast<tex::color>(foreground)).build(formula));
    int width=render->getWidth(), height=render->getHeight(), depth=render->getDepth();
    if (width < 1 || height < 1 || width > 4096 || height > 2048 || int64_t(width)*height > 4194304 || depth < 0 || depth > height) return 4;
    auto surface=Cairo::ImageSurface::create(Cairo::Surface::Format::ARGB32,width,height);
    auto context=Cairo::Context::create(surface);
    tex::Graphics2D_cairo graphics(context);
    render->draw(graphics,0,0);
    std::cout << depth << '\n'; std::cout.flush();
    auto status=cairo_surface_write_to_png_stream(surface->cobj(), [](void*, const unsigned char* bytes, unsigned int length) {
      return std::fwrite(bytes,1,length,stdout)==length ? CAIRO_STATUS_SUCCESS : CAIRO_STATUS_WRITE_ERROR;
    },nullptr);
    return status==CAIRO_STATUS_SUCCESS && std::fflush(stdout)==0 ? 0 : 5;
  } catch (...) { return 6; }
}
