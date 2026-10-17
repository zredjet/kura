/* 照合用のエンコーダ（aoTuV b6.03 の写し、KURA_PORT でビルド）。
   使い方: oracle_enc in.f32 out.pkt rate channels q [--vendor 文字列] [--drain-between] [--chunk N]
     in.f32  : planar の f32 リトルエンディアンの生データ（チャンネル 0 の全フレーム、
               チャンネル 1 の全フレーム、…）。フレーム数 = ファイルの大きさ / (4 × channels)
     out.pkt : パケットのファイル（README の「パケットのファイル」。ヘッダの 3 パケットを含む）
     q       : 整数 −1〜10（libvorbis の quality = (float)q / 10.0f で vorbis_encode_init_vbr）
   入力は全体を 1 回の vorbis_analysis_buffer・vorbis_analysis_wrote で渡し、そのあと wrote(0)、
   それからブロックを出し切る（--drain-between を付けると wrote(N) と wrote(0) の間でも出す。比較用）。
   --chunk N は確かめ専用：調査の enc.c と同じく N フレームずつ渡し、そのたびに出し切る
   （最初のパケットが変わるので照合には使わない。手を入れていない写しとの一致を確かめるため）。
   コメントのヘッダは vendor を空（--vendor で指定可）、タグなし。
   環境変数：KURA_DUMP=ファイル、KURA_DUMP_LEVEL=1|2（既定 1）でブロックごとの中間値を書く。
             KURA_ABL で機能の組を上書き（既定 PREAMP。前増幅だけを切る）。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <vorbis/vorbisenc.h>
#include "kura_port.h"

static void put_le(FILE *o, uint64_t v, int bytes){
  unsigned char b[8];
  for(int i=0;i<bytes;i++)b[i]=(unsigned char)(v>>(8*i));
  fwrite(b,1,bytes,o);
}

static long npk=0, nbytes=0;
static void write_packet(FILE *o, const ogg_packet *op){
  put_le(o,(uint64_t)(uint32_t)op->bytes,4);
  put_le(o,(uint64_t)op->granulepos,8);
  unsigned char fl=(op->b_o_s?1:0)|(op->e_o_s?2:0);
  fwrite(&fl,1,1,o);
  fwrite(op->packet,1,op->bytes,o);
  npk++; nbytes+=op->bytes;
}

static int drain(vorbis_dsp_state *vd, vorbis_block *vb, FILE *o){
  ogg_packet op;
  while(vorbis_analysis_blockout(vd,vb)==1){
    if(vorbis_analysis(vb,NULL))return -1;
    if(vorbis_bitrate_addblock(vb))return -1;
    while(vorbis_bitrate_flushpacket(vd,&op))write_packet(o,&op);
  }
  return 0;
}

int main(int argc,char **argv){
  if(argc<6){
    fprintf(stderr,"usage: oracle_enc in.f32 out.pkt rate channels q [--vendor STR] [--drain-between] [--chunk N]\n");
    return 2;
  }
  const char *inpath=argv[1], *outpath=argv[2];
  long rate=atol(argv[3]);
  int ch=atoi(argv[4]);
  int q=atoi(argv[5]);
  const char *vendor=NULL;
  int drain_between=0;
  long chunk=0;
  for(int a=6;a<argc;a++){
    if(!strcmp(argv[a],"--vendor") && a+1<argc){ vendor=argv[++a]; }
    else if(!strcmp(argv[a],"--drain-between")) drain_between=1;
    else if(!strcmp(argv[a],"--chunk") && a+1<argc) chunk=atol(argv[++a]);
    else { fprintf(stderr,"unknown option %s\n",argv[a]); return 2; }
  }
  if(ch<1||ch>255||rate<=0||q<-1||q>10){ fprintf(stderr,"bad rate/channels/q\n"); return 2; }

  FILE *f=fopen(inpath,"rb"); if(!f){ perror(inpath); return 1; }
  fseek(f,0,SEEK_END); long len=ftell(f); fseek(f,0,SEEK_SET);
  if(len%(4L*ch)){ fprintf(stderr,"input size %ld is not a multiple of 4*channels\n",len); return 1; }
  long frames=len/(4L*ch);
  unsigned char *raw=malloc(len>0?len:1);
  if(len>0 && fread(raw,1,len,f)!=(size_t)len){ fprintf(stderr,"read error\n"); return 1; }
  fclose(f);

  const char *dpath=getenv("KURA_DUMP");
  const char *dlev=getenv("KURA_DUMP_LEVEL");
  if(dpath && *dpath){
    if(kd_open(dpath,dlev?atoi(dlev):1)){ perror(dpath); return 1; }
  }

  vorbis_info vi; vorbis_info_init(&vi);
  float quality=(float)q/10.0f;
  int ret=vorbis_encode_init_vbr(&vi,ch,rate,quality);
  if(ret){ fprintf(stderr,"vorbis_encode_init_vbr failed: %d\n",ret); return 1; }
  vorbis_comment vc; vorbis_comment_init(&vc);
  if(vendor) vc.vendor=strdup(vendor);
  vorbis_dsp_state vd; vorbis_analysis_init(&vd,&vi);
  vorbis_block vb; vorbis_block_init(&vd,&vb);

  if(KD1){
    int64_t h[8]={1,kura_dump_level,rate,ch,q,kura_abl_mask(),frames,0};
    kd_i64("KDMP",KD_NOCH,h,8);
  }

  FILE *o=fopen(outpath,"wb"); if(!o){ perror(outpath); return 1; }
  ogg_packet h0,h1,h2;
  if(vorbis_analysis_headerout(&vd,&vc,&h0,&h1,&h2)){ fprintf(stderr,"headerout failed\n"); return 1; }
  write_packet(o,&h0); write_packet(o,&h1); write_packet(o,&h2);

  if(chunk>0){
    for(long pos=0;pos<frames;pos+=chunk){
      long m=frames-pos<chunk?frames-pos:chunk;
      float **b=vorbis_analysis_buffer(&vd,(int)m);
      for(int c=0;c<ch;c++){
        const unsigned char *s=raw+((size_t)c*frames+pos)*4;
        for(long i=0;i<m;i++){
          uint32_t u=(uint32_t)s[4*i]|((uint32_t)s[4*i+1]<<8)|((uint32_t)s[4*i+2]<<16)|((uint32_t)s[4*i+3]<<24);
          float v; memcpy(&v,&u,4);
          b[c][i]=v;
        }
      }
      vorbis_analysis_wrote(&vd,(int)m);
      if(drain(&vd,&vb,o)){ fprintf(stderr,"encode failed\n"); return 1; }
    }
  }else if(frames>0){
    float **b=vorbis_analysis_buffer(&vd,(int)frames);
    for(int c=0;c<ch;c++){
      const unsigned char *s=raw+(size_t)c*frames*4;
      for(long i=0;i<frames;i++){
        uint32_t u=(uint32_t)s[4*i]|((uint32_t)s[4*i+1]<<8)|((uint32_t)s[4*i+2]<<16)|((uint32_t)s[4*i+3]<<24);
        float v; memcpy(&v,&u,4);
        b[c][i]=v;
      }
    }
    vorbis_analysis_wrote(&vd,(int)frames);
    if(drain_between && drain(&vd,&vb,o)){ fprintf(stderr,"encode failed\n"); return 1; }
  }
  vorbis_analysis_wrote(&vd,0);
  if(drain(&vd,&vb,o)){ fprintf(stderr,"encode failed\n"); return 1; }
  fclose(o);

  fprintf(stderr,"oracle_enc: rate %ld ch %d q %d (quality %.9g) frames %ld blocksizes %d/%d abl 0x%x -> %ld packets, %ld bytes\n",
          rate,ch,q,(double)quality,frames,vorbis_info_blocksize(&vi,0),vorbis_info_blocksize(&vi,1),
          kura_abl_mask(),npk,nbytes);

  kd_close();
  vorbis_block_clear(&vb); vorbis_dsp_clear(&vd); vorbis_comment_clear(&vc); vorbis_info_clear(&vi);
  free(raw);
  return 0;
}
