// SPDX-License-Identifier: GPL-3.0-or-later
// Private bounded frame-pull helper. This process is launched only inside bwrap.
#include <rlottie_capi.h>
#include <zlib.h>
#include <errno.h>
#include <math.h>
#include <stddef.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#define INPUT_LIMIT (20u*1024u*1024u)
#define JSON_LIMIT (2u*1024u*1024u)
static int read_all(void *buffer,size_t size){unsigned char *p=buffer;while(size){ssize_t n=read(0,p,size);if(n<0&&errno==EINTR)continue;if(n<=0)return 0;p+=n;size-=(size_t)n;}return 1;}
static int write_all(const void *buffer,size_t size){const unsigned char *p=buffer;while(size){ssize_t n=write(1,p,size);if(n<0&&errno==EINTR)continue;if(n<=0)return 0;p+=n;size-=(size_t)n;}return 1;}
static uint32_t number(const unsigned char *p){return (uint32_t)p[0]|(uint32_t)p[1]<<8|(uint32_t)p[2]<<16|(uint32_t)p[3]<<24;}
static void encode(unsigned char *p,uint32_t n){p[0]=n;p[1]=n>>8;p[2]=n>>16;p[3]=n>>24;}
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
int main(void){
 if(!limit(RLIMIT_AS,512u*1024u*1024u)||!limit(RLIMIT_STACK,1024u*1024u)||!limit(RLIMIT_CORE,0)||!limit(RLIMIT_FSIZE,0)||!limit(RLIMIT_NOFILE,32)||prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)!=0)return 2;
 if(!restrict_calls())return 2;
 umask(077);alarm(8);
 unsigned char header[12];if(!read_all(header,sizeof header)||memcmp(header,"MGT1",4))return 3;
 uint32_t length=number(header+4),edge=number(header+8);if(!length||length>INPUT_LIMIT||!edge||edge>512)return 3;
 unsigned char *input=malloc(length);char *json=calloc(JSON_LIMIT+1,1);if(!input||!json||!read_all(input,length))return 3;
 z_stream stream;memset(&stream,0,sizeof stream);stream.next_in=input;stream.avail_in=length;stream.next_out=(unsigned char*)json;stream.avail_out=JSON_LIMIT+1;
 if(inflateInit2(&stream,16+MAX_WBITS)!=Z_OK)return 4;int status=inflate(&stream,Z_FINISH);int valid=status==Z_STREAM_END&&stream.total_out>0&&stream.total_out<=JSON_LIMIT&&stream.avail_in==0;size_t json_length=stream.total_out;inflateEnd(&stream);free(input);
 if(!valid||memchr(json,0,json_length))return 4;
 lottie_configure_model_cache_size(0);
 Lottie_Animation *animation=lottie_animation_from_data(json,"monogram-private-sticker","/tmp/empty/");free(json);if(!animation)return 5;
 size_t width=0,height=0;lottie_animation_get_size(animation,&width,&height);size_t frames=lottie_animation_get_totalframe(animation);
 // rlottie includes the out-point; TGS playback treats it as exclusive.
 if(frames>1)frames--;
 double fps=lottie_animation_get_framerate(animation);
 if(width!=512||height!=512||frames<1||frames>180||!isfinite(fps)||fps<=0||fps>60||((double)frames/fps)>3.001){lottie_animation_destroy(animation);return 5;}
 size_t size=(size_t)edge*edge*4;uint32_t *pixels=calloc(1,size);unsigned char *rgba=malloc(size);if(!pixels||!rgba)return 6;
 unsigned char metadata[20];memcpy(metadata,"MGF1",4);encode(metadata+4,edge);encode(metadata+8,edge);encode(metadata+12,(uint32_t)frames);encode(metadata+16,(uint32_t)lround(fps*1000));if(!write_all(metadata,sizeof metadata))return 7;alarm(0);
 unsigned char request[4];while(read_all(request,sizeof request)){
  uint32_t index=number(request);if(index>=frames)return 8;alarm(2);memset(pixels,0,size);lottie_animation_render(animation,index,pixels,edge,edge,edge*4);
  for(size_t i=0;i<size/4;i++){uint32_t p=pixels[i];rgba[4*i]=(p>>16)&255;rgba[4*i+1]=(p>>8)&255;rgba[4*i+2]=p&255;rgba[4*i+3]=(p>>24)&255;}
  unsigned char result[8];encode(result,index);encode(result+4,(uint32_t)size);if(!write_all(result,sizeof result)||!write_all(rgba,size))break;alarm(0);
 }
 free(rgba);free(pixels);lottie_animation_destroy(animation);return 0;
}
