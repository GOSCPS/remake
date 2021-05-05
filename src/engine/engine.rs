//=========================================================
// 这个文件来自 GOSCPS(https://github.com/GOSCPS)
// 使用 GOSCPS 许可证
// File:    engine.rs
// Content: pmake engine source file
// Copyright (c) 2020-2021 GOSCPS 保留所有权利.
//=========================================================

use crate::engine::error::RuntimeError;
use crate::engine::pfile::PFile;
use crate::engine::rule::Rule;
use crate::engine::target::Target;
use crate::Context;
use crate::Mutex;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// 全局
lazy_static! {
    // 全局Target列表
    pub static ref GLOBAL_TARGET_LIST
    : Mutex<HashMap<String,Target>> = Mutex::new(HashMap::new());

    // 全局Rule列表
    pub static ref GLOBAL_RULE_LIST
    : Mutex<HashMap<String,Rule>> = Mutex::new(HashMap::new());

    pub static ref FINISHED_TARGET_LIST
    : Mutex<Vec<String>> = Mutex::new(Vec::new());
}

pub fn execute_start(start: PFile) -> Result<(), RuntimeError> {
    // 检查重定义
    for rules in start.rules.into_iter() {
        if GLOBAL_RULE_LIST.lock().unwrap().contains_key(&rules.name) {
            return Err(RuntimeError {
                reason_token: None,
                reason_err: None,
                reason_str: Some(format!(
                    "The rule `{}` is defined at file `{:?}`!",
                    &rules.name, start.file
                )),
                help_str: None,
            });
        } else {
            GLOBAL_RULE_LIST
                .lock()
                .unwrap()
                .insert(rules.name.to_string(), rules);
        }
    }
    for target in start.targets.into_iter() {
        if GLOBAL_TARGET_LIST
            .lock()
            .unwrap()
            .contains_key(&target.name)
        {
            return Err(RuntimeError {
                reason_token: None,
                reason_err: None,
                reason_str: Some(format!(
                    "The rule `{}` is defined at file `{:?}`!",
                    &target.name, start.file
                )),
                help_str: None,
            });
        } else {
            GLOBAL_TARGET_LIST
                .lock()
                .unwrap()
                .insert(target.name.to_string(), target);
        }
    }

    // 执行全局语句
    match start.global_statements.execute(&mut Context::new()) {
        Err(err) => return Err(err),

        Ok(_ok) => (),
    }

    let mut aims: Vec<Target> = Vec::new();
    // 获取目标
    for aim_name in crate::TARGET_LIST.lock().unwrap().iter() {
        match GLOBAL_TARGET_LIST.lock().unwrap().get(aim_name) {
            Some(some) => {
                aims.push(some.clone());
            }

            None => {
                return Err(RuntimeError {
                    reason_token: None,
                    reason_err: None,
                    reason_str: Some(format!("Miss aim target `{}`!", aim_name)),
                    help_str: None,
                });
            }
        }
    }

    let temp = GLOBAL_TARGET_LIST
        .lock()
        .unwrap()
        .values()
        .clone()
        .cloned()
        .collect();
    // 依赖排序
    let mut what_we_will_build = crate::algorithm::topological::target_topological(&aims, &temp);

    // 多线程执行
    let (sender, receiver): (Sender<Arc<Target>>, Receiver<Arc<Target>>) = mpsc::channel();
    let (err_sender, err_receiver): (Sender<Arc<RuntimeError>>, Receiver<Arc<RuntimeError>>) =
        mpsc::channel();
    let mut thread_list = Vec::new();

    let task_receiver: Arc<Mutex<Receiver<Arc<Target>>>> = Arc::from(Mutex::new(receiver));

    // 启动线程
    for t in 0..*crate::BUILD_THREAD_COUNT.lock().unwrap() {
        {
            let err_sender = err_sender.clone();
            let receiver = task_receiver.clone();

            thread_list.push(
                thread::Builder::new()
                    .name(format!("Worker-{}", t))
                    .spawn(move || {
                        crate::tool::printer::ok_line(&format!(
                            "The {} started!",
                            thread::current().name().unwrap_or("UNKNOWN")
                        ));

                        // 任务循环
                        loop {
                            // 收集任务
                            match receiver.lock().unwrap().recv() {
                                Err(err) => {
                                    crate::tool::printer::ok_line(&format!(
                                        "The {} exit!",
                                        thread::current().name().unwrap_or("UNKNOWN")
                                    ));
                                    return;
                                }

                                Ok(ok) => {
                                    // 执行任务
                                    match ok.body.execute(&mut Context::new()) {
                                        Err(err) => {
                                            err.to_string();
                                            err_sender.send(Arc::new(err));
                                            crate::tool::printer::ok_line(&format!(
                                                "The {} build failed!",
                                                thread::current().name().unwrap_or("UNKNOWN")
                                            ));
                                        }

                                        // 添加到完成列表
                                        Ok(ok) => {
                                            FINISHED_TARGET_LIST
                                                .lock()
                                                .unwrap()
                                                .push(ok.name.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    })
                    .unwrap(),
            );
        }
    }

    // 发布任务
    loop {
        // 检查错误
        match err_receiver.try_recv() {
            // 有错误
            // 退出
            Ok(ok) => {
                // 关闭线程
                drop(sender);
                for t in thread_list {
                    t.join();
                }

                return Err(RuntimeError {
                    reason_token: None,
                    reason_err: None,
                    reason_str: Some("The worker build filed!".to_string()),
                    help_str: None,
                });
            }

            // 检查错误代码
            Err(err) => {
                match err {
                    // 空
                    // 继续
                    Empty => {}

                    // 管道被关闭
                    // 错误
                    Disconnected => {
                        crate::tool::printer::error_line(&format!(
                            "The error receiver disconnected!"
                        ));

                        // 关闭线程
                        drop(sender);
                        for t in thread_list {
                            t.join();
                        }

                        return Err(RuntimeError {
                            reason_token: None,
                            reason_err: None,
                            reason_str: Some("The error receiver disconnected!".to_string()),
                            help_str: None,
                        });
                    }
                }
            }
        }

        // 发布任务
        let task_map = what_we_will_build.pop_front();

        // 查看是否还有任务
        let task = match task_map {
            // 无任务
            None => {
                crate::tool::printer::debug_line("Done - wait thread exit");

                // 关闭线程
                drop(sender);
                for t in thread_list {
                    t.join();
                }

                return Ok(());
            }
            Some(some) => some,
        };

        // 检查依赖
        let mut finished_depends = true;

        for task_depend in &task.depends {
            if !FINISHED_TARGET_LIST.lock().unwrap().contains(task_depend) {
                finished_depends = false;
                break;
            }
        }

        // 只在依赖全部完成的时候发布任务
        if finished_depends {
            sender.send(Arc::from(<Target as Clone>::clone(task)));
        } else {
            // 送回队列
            what_we_will_build.push_back(task);
        }

        // 休眠
        // thread::sleep(Duration::new(0, 100000));
    }

    // 关闭线程
    drop(sender);
    for t in thread_list {
        t.join();
    }

    return Ok(());
}