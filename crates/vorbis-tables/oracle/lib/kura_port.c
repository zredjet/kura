/* 照合用のエンコーダ（KURA_PORT）：ダンプの書き出し。形式は oracle/README.md。
   記録 = tag[4]（ASCII）| ch: u16 | type: u16 | len: u32（値のバイト数）| 値
   type 1 = i32、2 = f32 のビット列（u32）、3 = i64。すべてリトルエンディアン。 */
#ifdef KURA_PORT
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include "kura_port.h"
#include "abl.h"

int kura_dump_level = 0;
int64_t kura_dump_seq = 0;
int kura_dump_ch = 0;
static FILE *kd_fp = NULL;

int kura_abl_mask(void){ return kura_abl(); }

int kd_open(const char *path, int level){
  if(!path || !*path || level<=0){ kura_dump_level=0; return 0; }
  kd_fp=fopen(path,"wb");
  if(!kd_fp){ kura_dump_level=0; return -1; }
  kura_dump_level=level>2?2:level;
  return 0;
}

void kd_close(void){
  if(kd_fp){ fclose(kd_fp); kd_fp=NULL; }
  kura_dump_level=0;
}

static void put_u16(uint16_t v){ unsigned char b[2]={(unsigned char)(v&255),(unsigned char)(v>>8)}; fwrite(b,1,2,kd_fp); }
static void put_u32(uint32_t v){ unsigned char b[4]; for(int i=0;i<4;i++)b[i]=(unsigned char)(v>>(8*i)); fwrite(b,1,4,kd_fp); }
static void put_u64(uint64_t v){ unsigned char b[8]; for(int i=0;i<8;i++)b[i]=(unsigned char)(v>>(8*i)); fwrite(b,1,8,kd_fp); }

static void head(const char *tag, int ch, int type, uint32_t len){
  char t[4]={' ',' ',' ',' '};
  for(int i=0;i<4 && tag[i];i++)t[i]=tag[i];
  fwrite(t,1,4,kd_fp);
  put_u16((uint16_t)ch);
  put_u16((uint16_t)type);
  put_u32(len);
}

void kd_i64(const char *tag, int ch, const int64_t *v, long n){
  if(!kd_fp)return;
  if(!v)n=0;
  head(tag,ch,3,(uint32_t)(n*8));
  for(long i=0;i<n;i++)put_u64((uint64_t)v[i]);
}

void kd_i32(const char *tag, int ch, const int *v, long n){
  if(!kd_fp)return;
  if(!v)n=0;
  head(tag,ch,1,(uint32_t)(n*4));
  for(long i=0;i<n;i++)put_u32((uint32_t)v[i]);
}

void kd_long(const char *tag, int ch, const long *v, long n){
  if(!kd_fp)return;
  if(!v)n=0;
  head(tag,ch,1,(uint32_t)(n*4));
  for(long i=0;i<n;i++)put_u32((uint32_t)(int32_t)v[i]);
}

void kd_f32(const char *tag, int ch, const float *v, long n){
  if(!kd_fp)return;
  if(!v)n=0;
  head(tag,ch,2,(uint32_t)(n*4));
  for(long i=0;i<n;i++){ uint32_t u; memcpy(&u,&v[i],4); put_u32(u); }
}
#endif
