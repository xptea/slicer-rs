/* OpenGL canvas and libmpv render-API boundary. All calls belong to one GL thread.
 * libmpv's stable render ABI is declared here; no internal mpv structs are used. */
#define GL_GLEXT_PROTOTYPES
#include <GL/gl.h>
#include <GL/glext.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <X11/Xlib.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <stdatomic.h>

typedef struct { int type; void *data; } Param;
typedef struct { void *(*get_proc_address)(void *, const char *); void *ctx; } Init;
typedef struct { int fbo,w,h,format; } Fbo;
typedef struct {
 Display *x; EGLDisplay display; EGLContext context; EGLSurface surface;
 void *library; GLuint program, vao, output_program, linear, linear_fbo;
 int width,height,offscreen;
 int (*create)(void**,void*,Param*); void (*destroy)(void*);
 uint64_t (*update)(void*); int (*render)(void*,Param*);
 void (*callback)(void*,void(*)(void*),void*); void (*swap)(void*);
} Canvas;
typedef struct { void *mpv; GLuint texture,fbo; int w,h; atomic_int dirty; } Source;
/* libmpv requires standard GL state at every render-API entry. */
static void reset_state(void) {
 glUseProgram(0);glBindVertexArray(0);glBindFramebuffer(GL_FRAMEBUFFER,0);
 glActiveTexture(GL_TEXTURE0);glBindTexture(GL_TEXTURE_2D,0);glBindSampler(0,0);
 glBindBuffer(GL_ARRAY_BUFFER,0);glBindBuffer(GL_PIXEL_UNPACK_BUFFER,0);glBindBuffer(GL_PIXEL_PACK_BUFFER,0);
 glPixelStorei(GL_UNPACK_ALIGNMENT,4);glPixelStorei(GL_PACK_ALIGNMENT,4);
 glPixelStorei(GL_UNPACK_ROW_LENGTH,0);glPixelStorei(GL_PACK_ROW_LENGTH,0);
 glDisable(GL_BLEND);glDisable(GL_SCISSOR_TEST);glDisable(GL_DEPTH_TEST);glDisable(GL_CULL_FACE);glDisable(GL_FRAMEBUFFER_SRGB);
}
static void notify(void *p){atomic_store(&((Source*)p)->dirty,1);}
static void *get_proc(void *unused,const char *name){(void)unused;return (void*)eglGetProcAddress(name);}
static GLuint shader(GLenum type,const char *text,char *error,int len){GLuint s=glCreateShader(type);glShaderSource(s,1,&text,NULL);glCompileShader(s);GLint ok;glGetShaderiv(s,GL_COMPILE_STATUS,&ok);if(!ok){glGetShaderInfoLog(s,len,NULL,error);glDeleteShader(s);return 0;}return s;}
static GLuint program(const char *vs,const char *fs,char *error,int len){GLuint v=shader(GL_VERTEX_SHADER,vs,error,len),f=shader(GL_FRAGMENT_SHADER,fs,error,len);if(!v||!f){if(v)glDeleteShader(v);if(f)glDeleteShader(f);return 0;}GLuint p=glCreateProgram();glAttachShader(p,v);glAttachShader(p,f);glLinkProgram(p);glDeleteShader(v);glDeleteShader(f);GLint ok;glGetProgramiv(p,GL_LINK_STATUS,&ok);if(!ok){glGetProgramInfoLog(p,len,NULL,error);glDeleteProgram(p);return 0;}return p;}
static const char *vs = "#version 330 core\n"
 "uniform vec4 rect;uniform vec2 viewport;uniform float angle;out vec2 uv;"
 "void main(){vec2 p[6]=vec2[6](vec2(0,0),vec2(1,0),vec2(0,1),vec2(0,1),vec2(1,0),vec2(1,1));uv=p[gl_VertexID];vec2 q=(uv-.5)*rect.zw;float c=cos(angle),s=sin(angle);vec2 xy=rect.xy+vec2(q.x*c-q.y*s,q.x*s+q.y*c);gl_Position=vec4(xy.x/viewport.x*2-1,1-xy.y/viewport.y*2,0,1);}";
static const char *fs = "#version 330 core\n"
 "uniform sampler2D tex;uniform float opacity;uniform int outline;uniform vec2 extent;in vec2 uv;out vec4 color;"
 "void main(){if(outline!=0){vec2 d=min(uv,1-uv)*extent;if(min(d.x,d.y)>2.0)discard;color=vec4(.14,.4,1,1);return;}vec4 p=texture(tex,uv);vec3 rgb=mix(p.rgb/12.92,pow((p.rgb+.055)/1.055,vec3(2.4)),greaterThan(p.rgb,vec3(.04045)));float a=p.a*opacity;color=vec4(rgb*a,a);}";
static const char *outfs = "#version 330 core\n"
 "uniform sampler2D tex;in vec2 uv;out vec4 color;void main(){vec3 p=texture(tex,vec2(uv.x,1-uv.y)).rgb;vec3 c=mix(p*12.92,1.055*pow(max(p,vec3(0)),vec3(1.0/2.4))-.055,greaterThan(p,vec3(.0031308)));color=vec4(c,1);}";
void slicer_gl_close(Canvas*c){if(!c)return;if(c->display!=EGL_NO_DISPLAY&&c->context!=EGL_NO_CONTEXT){eglMakeCurrent(c->display,c->surface,c->surface,c->context);glDeleteProgram(c->program);glDeleteProgram(c->output_program);glDeleteVertexArrays(1,&c->vao);glDeleteTextures(1,&c->linear);glDeleteFramebuffers(1,&c->linear_fbo);eglMakeCurrent(c->display,EGL_NO_SURFACE,EGL_NO_SURFACE,EGL_NO_CONTEXT);eglDestroyContext(c->display,c->context);}if(c->surface!=EGL_NO_SURFACE)eglDestroySurface(c->display,c->surface);if(c->display!=EGL_NO_DISPLAY)eglTerminate(c->display);if(c->x)XCloseDisplay(c->x);if(c->library)dlclose(c->library);free(c);}
Canvas *slicer_gl_open(uint64_t window,int w,int h,const char *library,char *error,int len){
 Canvas*c=calloc(1,sizeof(*c));if(!c)return NULL;c->offscreen=!window;c->width=w;c->height=h;
 c->library=dlopen(library,RTLD_NOW|RTLD_LOCAL);if(!c->library){snprintf(error,len,"libmpv: %s",dlerror());goto bad;}
 #define LOAD(field,name) c->field=dlsym(c->library,name);if(!c->field){snprintf(error,len,"libmpv missing %s",name);goto bad;}
 LOAD(create,"mpv_render_context_create");LOAD(destroy,"mpv_render_context_free");LOAD(update,"mpv_render_context_update");LOAD(render,"mpv_render_context_render");LOAD(callback,"mpv_render_context_set_update_callback");LOAD(swap,"mpv_render_context_report_swap");
 c->x=XOpenDisplay(NULL);if(!c->x){snprintf(error,len,"OpenGL canvas requires an X11 display");goto bad;}
 c->display=eglGetDisplay((EGLNativeDisplayType)c->x);if(!eglInitialize(c->display,NULL,NULL)||!eglBindAPI(EGL_OPENGL_API)){snprintf(error,len,"Initialize EGL OpenGL: 0x%x",eglGetError());goto bad;}
 EGLint attrs[]={EGL_SURFACE_TYPE,window?EGL_WINDOW_BIT:EGL_PBUFFER_BIT,EGL_RENDERABLE_TYPE,EGL_OPENGL_BIT,EGL_RED_SIZE,8,EGL_GREEN_SIZE,8,EGL_BLUE_SIZE,8,EGL_NONE};
 EGLConfig configs[128],config=NULL;EGLint count=0;if(!eglChooseConfig(c->display,attrs,configs,128,&count)||!count){snprintf(error,len,"No EGL OpenGL configuration");goto bad;}
 XWindowAttributes xa;if(window&&!XGetWindowAttributes(c->x,window,&xa)){snprintf(error,len,"Canvas window unavailable");goto bad;}
 for(int i=0;i<count;i++){EGLint visual;eglGetConfigAttrib(c->display,configs[i],EGL_NATIVE_VISUAL_ID,&visual);if(!window||(VisualID)visual==XVisualIDFromVisual(xa.visual)){config=configs[i];break;}}
 if(!config){snprintf(error,len,"No EGL configuration matches canvas window visual");goto bad;}
 EGLint context_attrs[]={EGL_CONTEXT_MAJOR_VERSION,3,EGL_CONTEXT_MINOR_VERSION,3,EGL_CONTEXT_OPENGL_PROFILE_MASK,EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,EGL_NONE};
 c->context=eglCreateContext(c->display,config,EGL_NO_CONTEXT,context_attrs);
 EGLint pb[]={EGL_WIDTH,w,EGL_HEIGHT,h,EGL_NONE};c->surface=window?eglCreateWindowSurface(c->display,config,(EGLNativeWindowType)window,NULL):eglCreatePbufferSurface(c->display,config,pb);
 if(c->context==EGL_NO_CONTEXT||c->surface==EGL_NO_SURFACE||!eglMakeCurrent(c->display,c->surface,c->surface,c->context)){snprintf(error,len,"Create OpenGL canvas: 0x%x",eglGetError());goto bad;}
 eglSwapInterval(c->display,0);c->program=program(vs,fs,error,len);c->output_program=program(vs,outfs,error,len);if(!c->program||!c->output_program)goto bad;
 glGenVertexArrays(1,&c->vao);glGenTextures(1,&c->linear);glGenFramebuffers(1,&c->linear_fbo);c->width=c->height=0;return c;
 bad:slicer_gl_close(c);return NULL;
}
Source *slicer_gl_source(Canvas*c,void *handle,char *error,int len){Source*s=calloc(1,sizeof(*s));if(!s)return NULL;atomic_init(&s->dirty,1);if(handle){reset_state();Init init={get_proc,NULL};Param params[]={{1,"opengl"},{2,&init},{8,c->x},{0,NULL}};int r=c->create(&s->mpv,handle,params);if(r<0){snprintf(error,len,"libmpv OpenGL context error %d",r);free(s);return NULL;}c->callback(s->mpv,notify,s);}glGenTextures(1,&s->texture);glGenFramebuffers(1,&s->fbo);return s;}
void slicer_gl_source_free(Canvas*c,Source*s){if(!s)return;if(s->mpv){reset_state();c->callback(s->mpv,NULL,NULL);c->destroy(s->mpv);}glDeleteTextures(1,&s->texture);glDeleteFramebuffers(1,&s->fbo);free(s);}
static void texture(GLuint tex,int w,int h,int format,const void*data){glBindTexture(GL_TEXTURE_2D,tex);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_LINEAR);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_LINEAR);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_S,GL_CLAMP_TO_EDGE);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_T,GL_CLAMP_TO_EDGE);glPixelStorei(GL_UNPACK_ALIGNMENT,1);glTexImage2D(GL_TEXTURE_2D,0,format,w,h,0,GL_RGBA,GL_UNSIGNED_BYTE,data);}
int slicer_gl_video(Canvas*c,Source*s,int w,int h){int resized=w!=s->w||h!=s->h;if(resized){texture(s->texture,w,h,GL_RGBA8,NULL);glBindFramebuffer(GL_FRAMEBUFFER,s->fbo);glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,s->texture,0);if(glCheckFramebufferStatus(GL_FRAMEBUFFER)!=GL_FRAMEBUFFER_COMPLETE)return -100;s->w=w;s->h=h;}
 reset_state();uint64_t flags=c->update(s->mpv);int dirty=atomic_exchange(&s->dirty,0);if(!resized&&!(flags&1)&&!dirty)return 0;
 Fbo f={(int)s->fbo,w,h,GL_RGBA8};int flip=0,block=0;Param params[]={{3,&f},{4,&flip},{12,&block},{0,NULL}};/* A resize/notification can render a placeholder before decoding completes.
    Only an actual queued frame may mark the source ready. */
 int r=c->render(s->mpv,params);return r<0?r:((flags&1)?1:0);}
void slicer_gl_image(Source*s,int w,int h,const uint8_t *rgba){texture(s->texture,w,h,GL_RGBA8,rgba);s->w=w;s->h=h;}
int slicer_gl_begin(Canvas*c,int w,int h){if(w<=0||h<=0)return -1;if(w!=c->width||h!=c->height){texture(c->linear,w,h,GL_RGBA16F,NULL);glBindFramebuffer(GL_FRAMEBUFFER,c->linear_fbo);glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,c->linear,0);if(glCheckFramebufferStatus(GL_FRAMEBUFFER)!=GL_FRAMEBUFFER_COMPLETE)return -1;c->width=w;c->height=h;}
 glBindFramebuffer(GL_FRAMEBUFFER,c->linear_fbo);glViewport(0,0,w,h);glDisable(GL_SCISSOR_TEST);glDisable(GL_DEPTH_TEST);glDisable(GL_CULL_FACE);glDisable(GL_FRAMEBUFFER_SRGB);glColorMask(1,1,1,1);glClearColor(0,0,0,1);glClear(GL_COLOR_BUFFER_BIT);glEnable(GL_BLEND);glBlendEquation(GL_FUNC_ADD);glBlendFunc(GL_ONE,GL_ONE_MINUS_SRC_ALPHA);glBindVertexArray(c->vao);return 0;}
static void uniforms(Canvas*c,GLuint p,const float*t){glUseProgram(p);glUniform4f(glGetUniformLocation(p,"rect"),t[0]*c->width,t[1]*c->height,t[2]*c->width,t[3]*c->height);glUniform2f(glGetUniformLocation(p,"viewport"),c->width,c->height);glUniform1f(glGetUniformLocation(p,"angle"),t[4]);glUniform1i(glGetUniformLocation(p,"tex"),0);}
void slicer_gl_draw(Canvas*c,Source*s,const float*t,int selected){uniforms(c,c->program,t);glUniform1f(glGetUniformLocation(c->program,"opacity"),t[5]);glUniform1i(glGetUniformLocation(c->program,"outline"),0);glActiveTexture(GL_TEXTURE0);glBindTexture(GL_TEXTURE_2D,s->texture);glDrawArrays(GL_TRIANGLES,0,6);if(selected){glUniform1i(glGetUniformLocation(c->program,"outline"),1);glUniform2f(glGetUniformLocation(c->program,"extent"),t[2]*c->width,t[3]*c->height);glDrawArrays(GL_TRIANGLES,0,6);}}
void slicer_gl_finish(Canvas*c){glBindFramebuffer(GL_FRAMEBUFFER,0);glViewport(0,0,c->width,c->height);glDisable(GL_BLEND);float t[]={.5,.5,1,1,0,1};uniforms(c,c->output_program,t);glActiveTexture(GL_TEXTURE0);glBindTexture(GL_TEXTURE_2D,c->linear);glDrawArrays(GL_TRIANGLES,0,6);glFlush();}
int slicer_gl_present(Canvas*c){return eglSwapBuffers(c->display,c->surface)?0:-1;}
void slicer_gl_report(Canvas*c,Source*s){if(s->mpv)c->swap(s->mpv);}
void slicer_gl_read(Canvas*c,uint8_t *pixels){glPixelStorei(GL_PACK_ALIGNMENT,1);glReadPixels(0,0,c->width,c->height,GL_RGBA,GL_UNSIGNED_BYTE,pixels);}
