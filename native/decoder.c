/* Small owned FFmpeg ABI boundary. No AV* layout is reproduced in Rust. */
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libavutil/channel_layout.h>
#include <libswscale/swscale.h>
#include <libswresample/swresample.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#if LIBAVCODEC_VERSION_MAJOR != 62
#error Slicer requires the FFmpeg 8 ABI (libavcodec 62)
#endif

typedef struct {
    AVFormatContext *format; AVCodecContext *codec; AVPacket *packet;
    AVFrame *frame, *current, *next; int stream, draining, audio;
    struct SwsContext *sws; SwrContext *swr;
    uint8_t *pixels; int capacity;
    float *samples; int sample_capacity, sample_count; int64_t sample_start;
    int64_t current_us, next_us; int have_current, have_next;
    char error[256]; int (*cancel)(void*); void *cancel_data; int preview_size;
} Decoder;
typedef struct {
    int width,height,rgba,full_range,matrix,transfer,primaries; int64_t pts_us,duration_us;
    const uint8_t *data; int length;
} Video;
typedef struct {int width,height,video,audio; int64_t duration_us;} Info;
static int fail(Decoder *d,int code,const char *where){char buf[128];av_strerror(code,buf,sizeof(buf));snprintf(d->error,sizeof(d->error),"%s: %s",where,buf);return -1;}
void slicer_decoder_close(Decoder *d){if(!d)return;av_frame_free(&d->frame);av_frame_free(&d->current);av_frame_free(&d->next);av_packet_free(&d->packet);avcodec_free_context(&d->codec);avformat_close_input(&d->format);sws_freeContext(d->sws);swr_free(&d->swr);av_free(d->pixels);av_free(d->samples);free(d);}
Decoder *slicer_decoder_open(const char *path,int audio,char *error,int error_len,Info *info){
    Decoder *d=calloc(1,sizeof(*d)); if(!d)return NULL; d->audio=audio;
    int r=avformat_open_input(&d->format,path,NULL,NULL);if(r<0){fail(d,r,"Open media");goto bad;}
    r=avformat_find_stream_info(d->format,NULL);if(r<0){fail(d,r,"Inspect media");goto bad;}
    memset(info,0,sizeof(*info));info->duration_us=d->format->duration>0?d->format->duration:0;
    for(unsigned i=0;i<d->format->nb_streams;i++){AVCodecParameters *p=d->format->streams[i]->codecpar;if(p->codec_type==AVMEDIA_TYPE_VIDEO){info->video=1;info->width=p->width;info->height=p->height;}if(p->codec_type==AVMEDIA_TYPE_AUDIO)info->audio=1;}
    const AVCodec *codec=NULL;d->stream=av_find_best_stream(d->format,audio?AVMEDIA_TYPE_AUDIO:AVMEDIA_TYPE_VIDEO,-1,-1,&codec,0);
    if(d->stream<0){fail(d,d->stream,"Find stream");goto bad;}
    d->codec=avcodec_alloc_context3(codec);if(!d->codec)goto bad;
    avcodec_parameters_to_context(d->codec,d->format->streams[d->stream]->codecpar);d->codec->thread_count=2;
    if((r=avcodec_open2(d->codec,codec,NULL))<0){fail(d,r,"Open decoder");goto bad;}
    d->frame=av_frame_alloc();d->current=av_frame_alloc();d->next=av_frame_alloc();d->packet=av_packet_alloc();
    if(!d->frame||!d->current||!d->next||!d->packet)goto bad;
    if(audio){AVChannelLayout stereo=AV_CHANNEL_LAYOUT_STEREO;
        r=swr_alloc_set_opts2(&d->swr,&stereo,AV_SAMPLE_FMT_FLT,48000,&d->codec->ch_layout,d->codec->sample_fmt,d->codec->sample_rate,0,NULL);
        if(r<0 || (r=swr_init(d->swr))<0){fail(d,r,"Audio resampler");goto bad;}}
    return d;
bad:snprintf(error,error_len,"%s",d->error[0]?d->error:"Decoder allocation failed");slicer_decoder_close(d);return NULL;
}
const char *slicer_decoder_error(Decoder*d){return d->error;}
void slicer_decoder_preview(Decoder*d,int size){d->preview_size=size;}
static int64_t pts(Decoder*d,AVFrame*f){int64_t p=f->best_effort_timestamp;if(p==AV_NOPTS_VALUE)p=f->pts;if(p==AV_NOPTS_VALUE)return 0;AVStream*s=d->format->streams[d->stream];int64_t origin=d->format->start_time!=AV_NOPTS_VALUE?d->format->start_time:0;return av_rescale_q(p,s->time_base,AV_TIME_BASE_Q)-origin;}
static int seek_to(Decoder*d,int64_t time){AVStream*s=d->format->streams[d->stream];int64_t origin=d->format->start_time!=AV_NOPTS_VALUE?d->format->start_time:0;int64_t ts=av_rescale_q_rnd(time+origin,AV_TIME_BASE_Q,s->time_base,AV_ROUND_DOWN);int r=av_seek_frame(d->format,d->stream,ts,AVSEEK_FLAG_BACKWARD);if(r<0)return fail(d,r,"Seek");avcodec_flush_buffers(d->codec);d->draining=0;d->have_current=d->have_next=0;d->sample_count=0;av_frame_unref(d->current);av_frame_unref(d->next);if(d->swr){swr_close(d->swr);swr_init(d->swr);}return 0;}
void slicer_decoder_interrupt(Decoder*d,int(*callback)(void*),void*data){d->cancel=callback;d->cancel_data=data;}
static int read_frame(Decoder*d,AVFrame*f){
    for(;;){if(d->cancel && d->cancel(d->cancel_data))return fail(d,AVERROR_EXIT,"Decode superseded");int r=avcodec_receive_frame(d->codec,f);if(r==0)return 1;if(r==AVERROR_EOF)return 0;if(r!=AVERROR(EAGAIN))return fail(d,r,"Decode");
        if(d->draining)return 0;
        do {r=av_read_frame(d->format,d->packet);if(r<0){d->draining=1;avcodec_send_packet(d->codec,NULL);break;}
            if(d->packet->stream_index!=d->stream)av_packet_unref(d->packet);
            else break;
        }while(1);
        if(!d->draining){r=avcodec_send_packet(d->codec,d->packet);av_packet_unref(d->packet);if(r<0)return fail(d,r,"Submit packet");}
    }
}
int slicer_decoder_video(Decoder*d,int64_t time,Video*out){
    /* libmpv and ffmpeg's proxy transcode apply display matrices. Do not show
       an unrotated software fallback before the proxy is ready. */
    if(d->preview_size){AVCodecParameters*p=d->format->streams[d->stream]->codecpar;
        for(int i=0;i<p->nb_coded_side_data;i++)if(p->coded_side_data[i].type==AV_PKT_DATA_DISPLAYMATRIX){snprintf(d->error,sizeof(d->error),"Preparing oriented preview");return -1;}}
    if((!d->have_current && time>0) || (d->have_current && (time<d->current_us || time>d->current_us+1000000))){if(seek_to(d,time)<0)return -1;}
    if(!d->have_current){int r=read_frame(d,d->current);if(r<=0)return r;d->current_us=pts(d,d->current);d->have_current=1;}
    for(;;){if(!d->have_next){int r=read_frame(d,d->next);if(r<0)return -1;if(!r)break;d->next_us=pts(d,d->next);d->have_next=1;}
        if(d->next_us>time)break;
        av_frame_unref(d->current);av_frame_move_ref(d->current,d->next);d->current_us=d->next_us;d->have_next=0;
    }
    AVFrame*f=d->current;
    if(f->color_trc==AVCOL_TRC_SMPTE2084 || f->color_trc==AVCOL_TRC_ARIB_STD_B67){snprintf(d->error,sizeof(d->error),"HDR media requires a tone-mapping backend; this preview supports SDR");return -1;}
    const AVPixFmtDescriptor*desc=av_pix_fmt_desc_get(f->format);
    int rgba=d->preview_size || (desc && (desc->flags & (AV_PIX_FMT_FLAG_RGB|AV_PIX_FMT_FLAG_ALPHA)));
    enum AVPixelFormat target=rgba?AV_PIX_FMT_RGBA:AV_PIX_FMT_YUV420P;
    int w=f->width,h=f->height;
    if(d->preview_size && (w>d->preview_size || h>d->preview_size)){double scale=(double)d->preview_size/(w>h?w:h);w=(int)(w*scale);h=(int)(h*scale);if(w<1)w=1;if(h<1)h=1;}
    int len=av_image_get_buffer_size(target,w,h,1);if(len<=0||f->width>8192||f->height>8192)return fail(d,AVERROR(EINVAL),"Frame dimensions");
    /* swscale's SIMD stores can extend beyond the final packed row. */
    if(len>d->capacity){av_free(d->pixels);d->pixels=av_mallocz(len + AV_INPUT_BUFFER_PADDING_SIZE);d->capacity=len;}if(!d->pixels)return -1;
    uint8_t*planes[4];int strides[4];av_image_fill_arrays(planes,strides,d->pixels,target,w,h,1);
    if(w==f->width && h==f->height && (f->format==target || (!rgba && f->format==AV_PIX_FMT_YUVJ420P))){av_image_copy(planes,strides,(const uint8_t**)f->data,f->linesize,target,w,h);}
    else {d->sws=sws_getCachedContext(d->sws,f->width,f->height,f->format,w,h,target,SWS_BILINEAR,NULL,NULL,NULL);if(!d->sws)return -1;
        const int*coeff=sws_getCoefficients(f->colorspace==AVCOL_SPC_BT709?SWS_CS_ITU709:SWS_CS_ITU601);
        sws_setColorspaceDetails(d->sws,coeff,f->color_range==AVCOL_RANGE_JPEG,coeff,rgba?1:f->color_range==AVCOL_RANGE_JPEG,0,1<<16,1<<16);
        sws_scale(d->sws,(const uint8_t*const*)f->data,f->linesize,0,f->height,planes,strides);}
    *out=(Video){.width=w,.height=h,.rgba=rgba,.full_range=(rgba || f->color_range==AVCOL_RANGE_JPEG || f->format==AV_PIX_FMT_YUVJ420P),.matrix=f->colorspace,.transfer=f->color_trc,.primaries=f->color_primaries,.pts_us=d->current_us,.duration_us=d->have_next?d->next_us-d->current_us:(f->duration>0?av_rescale_q(f->duration,d->format->streams[d->stream]->time_base,AV_TIME_BASE_Q):33333),.data=d->pixels,.length=len};return 1;
}
/* Timestamp-addressed, stereo float output. Gaps and EOF are silence. */
int slicer_decoder_audio(Decoder*d,int64_t time,float*out,int count){
    memset(out,0,count*2*sizeof(float));int64_t wanted=av_rescale(time,48000,1000000);
    if((!d->sample_count && time>0) || (d->sample_count && (wanted<d->sample_start || wanted>d->sample_start+d->sample_count+48000))){if(seek_to(d,time)<0)return -1;}
    int written=0;
    while(written<count){
        if(d->sample_count && wanted>=d->sample_start && wanted<d->sample_start+d->sample_count){int offset=(int)(wanted-d->sample_start);int n=d->sample_count-offset;if(n>count-written)n=count-written;memcpy(out+written*2,d->samples+offset*2,n*2*sizeof(float));written+=n;wanted+=n;continue;}
        if(d->sample_count && wanted<d->sample_start){int64_t n=d->sample_start-wanted;if(n>count-written)n=count-written;written+=(int)n;wanted+=n;continue;}
        av_frame_unref(d->frame);int r=read_frame(d,d->frame);if(r<0)return -1;if(!r)break;
        int capacity=swr_get_out_samples(d->swr,d->frame->nb_samples);if(capacity>d->sample_capacity){av_free(d->samples);d->samples=av_malloc(capacity*2*sizeof(float));d->sample_capacity=capacity;}if(!d->samples)return -1;
        int64_t delay=swr_get_delay(d->swr,48000);uint8_t*dst=(uint8_t*)d->samples;
        d->sample_count=swr_convert(d->swr,&dst,capacity,(const uint8_t**)d->frame->extended_data,d->frame->nb_samples);
        if(d->sample_count<0)return fail(d,d->sample_count,"Resample");
        d->sample_start=av_rescale(pts(d,d->frame),48000,1000000)-delay;
    }return 0;
}

/* Offline encoder: preview and export share GPU composition; only export reads pixels back. */
typedef struct {
 AVFormatContext *format; AVCodecContext *video,*audio; AVStream *vs,*as; AVFrame *vf,*af;
 AVPacket *packet; struct SwsContext *sws; int64_t video_pts,audio_pts;
 float pending[2048]; int pending_count; char error[256];
} Encoder;
static int enc_fail(Encoder*e,int r,const char*where){char b[128];av_strerror(r,b,sizeof(b));snprintf(e->error,sizeof(e->error),"%s: %s",where,b);return -1;}
void slicer_encoder_close(Encoder*e){if(!e)return;av_frame_free(&e->vf);av_frame_free(&e->af);av_packet_free(&e->packet);avcodec_free_context(&e->video);avcodec_free_context(&e->audio);sws_freeContext(e->sws);if(e->format){if(e->format->pb)avio_closep(&e->format->pb);avformat_free_context(e->format);}free(e);}
const char*slicer_encoder_error(Encoder*e){return e->error;}
Encoder*slicer_encoder_open(const char*path,int w,int h,int fps_num,int fps_den,char*error,int len){
 Encoder*e=calloc(1,sizeof(*e));if(!e)return NULL;int r=avformat_alloc_output_context2(&e->format,NULL,"mp4",path);if(r<0)goto bad;
 const AVCodec*vc=avcodec_find_encoder(AV_CODEC_ID_MPEG4);const AVCodec*ac=avcodec_find_encoder(AV_CODEC_ID_AAC);if(!vc||!ac){snprintf(e->error,sizeof(e->error),"MPEG-4/AAC encoders unavailable");goto bad;}
 e->video=avcodec_alloc_context3(vc);e->audio=avcodec_alloc_context3(ac);e->vs=avformat_new_stream(e->format,NULL);e->as=avformat_new_stream(e->format,NULL);if(!e->video||!e->audio||!e->vs||!e->as)goto bad;
 e->video->width=w;e->video->height=h;e->video->pix_fmt=AV_PIX_FMT_YUV420P;e->video->time_base=(AVRational){fps_den,fps_num};e->video->framerate=(AVRational){fps_num,fps_den};e->video->bit_rate=(int64_t)w*h*6;e->video->gop_size=12;e->video->thread_count=2;e->video->color_range=AVCOL_RANGE_MPEG;e->video->colorspace=AVCOL_SPC_BT709;e->video->color_primaries=AVCOL_PRI_BT709;e->video->color_trc=AVCOL_TRC_IEC61966_2_1;
 e->audio->sample_fmt=AV_SAMPLE_FMT_FLTP;e->audio->sample_rate=48000;e->audio->time_base=(AVRational){1,48000};e->audio->ch_layout=(AVChannelLayout)AV_CHANNEL_LAYOUT_STEREO;e->audio->bit_rate=192000;
 if(e->format->oformat->flags&AVFMT_GLOBALHEADER){e->video->flags|=AV_CODEC_FLAG_GLOBAL_HEADER;e->audio->flags|=AV_CODEC_FLAG_GLOBAL_HEADER;}
 if((r=avcodec_open2(e->video,vc,NULL))<0 || (r=avcodec_open2(e->audio,ac,NULL))<0){enc_fail(e,r,"Open encoder");goto bad;}
 e->vs->time_base=e->video->time_base;e->as->time_base=e->audio->time_base;avcodec_parameters_from_context(e->vs->codecpar,e->video);avcodec_parameters_from_context(e->as->codecpar,e->audio);
 e->vf=av_frame_alloc();e->af=av_frame_alloc();e->packet=av_packet_alloc();if(!e->vf||!e->af||!e->packet)goto bad;
 e->vf->width=w;e->vf->height=h;e->vf->format=AV_PIX_FMT_YUV420P;e->af->format=AV_SAMPLE_FMT_FLTP;e->af->sample_rate=48000;e->af->nb_samples=e->audio->frame_size;av_channel_layout_copy(&e->af->ch_layout,&e->audio->ch_layout);
 if(e->af->nb_samples!=1024){snprintf(e->error,sizeof(e->error),"Unsupported AAC frame size");goto bad;}
 if((r=av_frame_get_buffer(e->vf,32))<0||(r=av_frame_get_buffer(e->af,0))<0){enc_fail(e,r,"Allocate encoder frames");goto bad;}
 e->sws=sws_getContext(w,h,AV_PIX_FMT_RGBA,w,h,AV_PIX_FMT_YUV420P,SWS_BILINEAR,NULL,NULL,NULL);if(!e->sws)goto bad;
 const int*coeff=sws_getCoefficients(SWS_CS_ITU709);sws_setColorspaceDetails(e->sws,coeff,1,coeff,0,0,1<<16,1<<16);
 if((r=avio_open(&e->format->pb,path,AVIO_FLAG_WRITE))<0||(r=avformat_write_header(e->format,NULL))<0){enc_fail(e,r,"Open export");goto bad;}return e;
 bad:snprintf(error,len,"%s",e->error[0]?e->error:"Could not initialize encoder");slicer_encoder_close(e);return NULL;
}
static int encode(Encoder*e,AVCodecContext*c,AVStream*s,AVFrame*f){int r=avcodec_send_frame(c,f);if(r<0)return enc_fail(e,r,"Encode");while((r=avcodec_receive_packet(c,e->packet))>=0){av_packet_rescale_ts(e->packet,c->time_base,s->time_base);e->packet->stream_index=s->index;r=av_interleaved_write_frame(e->format,e->packet);av_packet_unref(e->packet);if(r<0)return enc_fail(e,r,"Write export");}return r==AVERROR(EAGAIN)||r==AVERROR_EOF?0:enc_fail(e,r,"Receive encoded packet");}
int slicer_encoder_video(Encoder*e,const uint8_t*rgba){int r=av_frame_make_writable(e->vf);if(r<0)return enc_fail(e,r,"Writable frame");const uint8_t*src[]={rgba};int stride[]={e->video->width*4};sws_scale(e->sws,src,stride,0,e->video->height,e->vf->data,e->vf->linesize);e->vf->pts=e->video_pts++;return encode(e,e->video,e->vs,e->vf);}
static int audio_frame(Encoder*e){int r=av_frame_make_writable(e->af);if(r<0)return enc_fail(e,r,"Writable audio");for(int i=0;i<1024;i++){((float*)e->af->data[0])[i]=e->pending[i*2];((float*)e->af->data[1])[i]=e->pending[i*2+1];}e->af->pts=e->audio_pts;e->audio_pts+=1024;e->pending_count=0;return encode(e,e->audio,e->as,e->af);}
int slicer_encoder_audio(Encoder*e,const float*samples,int count){while(count>0){int n=1024-e->pending_count;if(n>count)n=count;memcpy(e->pending+e->pending_count*2,samples,n*2*sizeof(float));e->pending_count+=n;samples+=n*2;count-=n;if(e->pending_count==1024&&audio_frame(e)<0)return -1;}return 0;}
int slicer_encoder_finish(Encoder*e){if(e->pending_count){memset(e->pending+e->pending_count*2,0,(1024-e->pending_count)*2*sizeof(float));if(audio_frame(e)<0)return -1;}if(encode(e,e->video,e->vs,NULL)<0||encode(e,e->audio,e->as,NULL)<0)return -1;int r=av_write_trailer(e->format);if(r<0)return enc_fail(e,r,"Finalize export");return 0;}
