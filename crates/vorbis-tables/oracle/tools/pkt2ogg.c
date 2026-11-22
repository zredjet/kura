/* パケットのファイル（oracle_enc の出力、README の形式）を Ogg に包む。デコーダで読むための道具。
   使い方: pkt2ogg in.pkt out.ogg [serial] [--first-alone] [--last-alone]（serial の既定は 1）
   ページの切り方は libogg のまま：ident だけの最初のページ、comment と setup で 1 ページ（flush）、
   音声は ogg_stream_pageout（4,096 バイトを超えて 4 パケット以上など）、最後に flush。
   --first-alone：最初の音声パケットだけで 1 ページにする（調べもの用。デコーダの長さの扱いを見るため）
   --last-alone ：最後の音声パケットの前でページを閉じ、最後のパケットだけのページにする（同上） */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <ogg/ogg.h>

static int rd(FILE *f, void *p, size_t n){ return fread(p,1,n,f)==n; }
static uint64_t le(const unsigned char *b, int n){ uint64_t v=0; for(int i=n-1;i>=0;i--)v=(v<<8)|b[i]; return v; }

static void outpage(FILE *o, ogg_page *og){ fwrite(og->header,1,og->header_len,o); fwrite(og->body,1,og->body_len,o); }

int main(int argc, char **argv){
  if(argc<3){ fprintf(stderr,"usage: pkt2ogg in.pkt out.ogg [serial]\n"); return 2; }
  FILE *f=fopen(argv[1],"rb"); if(!f){ perror(argv[1]); return 1; }
  FILE *o=fopen(argv[2],"wb"); if(!o){ perror(argv[2]); return 1; }
  int serial=1, first_alone=0, last_alone=0;
  for(int a=3;a<argc;a++){
    if(!strcmp(argv[a],"--first-alone"))first_alone=1;
    else if(!strcmp(argv[a],"--last-alone"))last_alone=1;
    else serial=atoi(argv[a]);
  }
  ogg_stream_state os; ogg_stream_init(&os,serial);
  ogg_page og;
  long idx=0;
  for(;;){
    unsigned char h[13];
    if(!rd(f,h,13))break;
    uint32_t len=(uint32_t)le(h,4);
    int64_t gp=(int64_t)le(h+4,8);
    unsigned char fl=h[12];
    unsigned char *body=malloc(len?len:1);
    if(len && !rd(f,body,len)){ fprintf(stderr,"truncated packet %ld\n",idx); return 1; }
    ogg_packet op; memset(&op,0,sizeof(op));
    if(last_alone && idx>3 && (fl&2)){ while(ogg_stream_flush(&os,&og))outpage(o,&og); }
    op.packet=body; op.bytes=len; op.b_o_s=fl&1; op.e_o_s=(fl>>1)&1; op.granulepos=gp; op.packetno=idx;
    ogg_stream_packetin(&os,&op);
    free(body);
    if(idx==0 || idx==2 || (first_alone && idx==3)){
      while(ogg_stream_flush(&os,&og))outpage(o,&og);
    }else if(idx>2){
      while(ogg_stream_pageout(&os,&og))outpage(o,&og);
    }
    idx++;
  }
  while(ogg_stream_flush(&os,&og))outpage(o,&og);
  ogg_stream_clear(&os);
  fclose(o); fclose(f);
  fprintf(stderr,"pkt2ogg: %ld packets\n",idx);
  return 0;
}
