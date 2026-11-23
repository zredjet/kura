/* 照合用のデコーダ（M16 段 2・ADR-217）：libogg と libvorbis で Ogg Vorbis を読み、合成した PCM（float）をそのまま書く。
   使い方: oracle_dec in.ogg out.pcm
   流れは libvorbis の examples/decoder_example.c と同じ（最初の論理ストリームの 3 つのヘッダ → vorbis_synthesis_init →
   パケットごとに vorbis_synthesis が 0 なら vorbis_synthesis_blockin → vorbis_synthesis_pcmout / vorbis_synthesis_read）。
   連結・多重化は扱わない（照合の入力は 1 つの論理ストリームのファイルだけ）。
   出力：'KDEC'、チャンネルの数（u32 LE）、レート（u32 LE）、そのあとに標本（f32 のビットを LE で、時刻の順・Vorbis の
   チャンネルの順に交互）。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <ogg/ogg.h>
#include <vorbis/codec.h>

static void put_u32(FILE *o, uint32_t v){
  unsigned char b[4];
  for(int i=0;i<4;i++)b[i]=(unsigned char)(v>>(8*i));
  fwrite(b,1,4,o);
}

int main(int argc, char **argv){
  if(argc<3){ fprintf(stderr,"usage: oracle_dec in.ogg out.pcm\n"); return 2; }
  FILE *f=fopen(argv[1],"rb"); if(!f){ perror(argv[1]); return 1; }
  FILE *o=fopen(argv[2],"wb"); if(!o){ perror(argv[2]); return 1; }
  ogg_sync_state oy; ogg_sync_init(&oy);
  ogg_stream_state os; int have_stream=0;
  ogg_page og; ogg_packet op;
  vorbis_info vi; vorbis_info_init(&vi);
  vorbis_comment vc; vorbis_comment_init(&vc);
  vorbis_dsp_state vd; vorbis_block vb;
  int headers=0, eof=0;
  while(!eof){
    char *buf=ogg_sync_buffer(&oy,65536);
    size_t got=fread(buf,1,65536,f);
    ogg_sync_wrote(&oy,(long)got);
    if(got==0)eof=1;
    while(ogg_sync_pageout(&oy,&og)==1){
      if(!have_stream){ ogg_stream_init(&os,ogg_page_serialno(&og)); have_stream=1; }
      if(ogg_page_serialno(&og)!=os.serialno)continue;
      ogg_stream_pagein(&os,&og);
      while(ogg_stream_packetout(&os,&op)==1){
        if(headers<3){
          if(vorbis_synthesis_headerin(&vi,&vc,&op)<0){ fprintf(stderr,"oracle_dec: bad header\n"); return 1; }
          headers++;
          if(headers==3){
            if(vorbis_synthesis_init(&vd,&vi)){ fprintf(stderr,"oracle_dec: init failed\n"); return 1; }
            vorbis_block_init(&vd,&vb);
            fwrite("KDEC",1,4,o);
            put_u32(o,(uint32_t)vi.channels);
            put_u32(o,(uint32_t)vi.rate);
          }
          continue;
        }
        if(vorbis_synthesis(&vb,&op)==0)vorbis_synthesis_blockin(&vd,&vb);
        float **pcm; int samples;
        while((samples=vorbis_synthesis_pcmout(&vd,&pcm))>0){
          for(int i=0;i<samples;i++)
            for(int c=0;c<vi.channels;c++){
              uint32_t bits; memcpy(&bits,&pcm[c][i],4);
              put_u32(o,bits);
            }
          vorbis_synthesis_read(&vd,samples);
        }
      }
    }
  }
  if(headers<3){ fprintf(stderr,"oracle_dec: missing headers\n"); return 1; }
  fclose(o);
  return 0;
}
