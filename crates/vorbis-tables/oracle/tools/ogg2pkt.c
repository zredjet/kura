/* Ogg（最初の論理ストリームだけ）からパケットを取り出して、パケットのファイル（README の形式）にする。
   使い方: ogg2pkt in.ogg out.pkt
   ほかのエンコーダ（調査の enc_aotuv_nofma・vorbis_rs など）の出力と照らすための道具。
   granulepos はページの最後のパケットにしか載らないので、それ以外のパケットは -1 になる
   （oracle_enc の出力と比べるときは pktcmp.py の granulepos の違いは無視してよい）。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <ogg/ogg.h>

static void put_le(FILE *o, uint64_t v, int bytes){
  unsigned char b[8];
  for(int i=0;i<bytes;i++)b[i]=(unsigned char)(v>>(8*i));
  fwrite(b,1,bytes,o);
}

int main(int argc, char **argv){
  if(argc<3){ fprintf(stderr,"usage: ogg2pkt in.ogg out.pkt\n"); return 2; }
  FILE *f=fopen(argv[1],"rb"); if(!f){ perror(argv[1]); return 1; }
  FILE *o=fopen(argv[2],"wb"); if(!o){ perror(argv[2]); return 1; }
  ogg_sync_state oy; ogg_sync_init(&oy);
  ogg_stream_state os; int have_stream=0;
  ogg_page og; ogg_packet op;
  long n=0;
  for(;;){
    char *buf=ogg_sync_buffer(&oy,65536);
    size_t got=fread(buf,1,65536,f);
    ogg_sync_wrote(&oy,(long)got);
    while(ogg_sync_pageout(&oy,&og)==1){
      if(!have_stream){ ogg_stream_init(&os,ogg_page_serialno(&og)); have_stream=1; }
      if(ogg_page_serialno(&og)!=os.serialno)continue;
      ogg_stream_pagein(&os,&og);
      while(ogg_stream_packetout(&os,&op)==1){
        put_le(o,(uint64_t)(uint32_t)op.bytes,4);
        put_le(o,(uint64_t)op.granulepos,8);
        unsigned char fl=(op.b_o_s?1:0)|(op.e_o_s?2:0);
        fwrite(&fl,1,1,o);
        fwrite(op.packet,1,op.bytes,o);
        n++;
      }
    }
    if(got==0)break;
  }
  if(have_stream)ogg_stream_clear(&os);
  ogg_sync_clear(&oy);
  fclose(o); fclose(f);
  fprintf(stderr,"ogg2pkt: %ld packets\n",n);
  return 0;
}
